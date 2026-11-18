// Command go-proton-api runs Proton's development API server, seeds one
// synthetic account, and prints one JSON line the Rust lane test reads
// (plan M2.17, spec/11). It never contacts Proton: the server is the
// pinned development server in this directory's module, on 127.0.0.1.
// The process blocks until it is killed or stdin closes.
package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"math/big"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/ProtonMail/gluon/async"
	"github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/go-proton-api/server"
	"github.com/ProtonMail/go-srp"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
	"github.com/bradenaw/juniper/stream"
)

const (
	defaultUsername = "laneuser"
	defaultPassword = "lane-password"
)

// One seeded message, as the Rust lane asserts it: the server's message
// id (the store's provider key), its subject, the mailboxes it should
// appear in, and its unread state.
type seededMessage struct {
	ProviderKey string   `json:"provider_key"`
	Subject     string   `json:"subject"`
	LabelNames  []string `json:"labels"`
	Unread      bool     `json:"unread"`
}

// The one JSON line the lane reads.
type report struct {
	BaseURL      string          `json:"base_url"`
	CertPEM      string          `json:"cert_pem"`
	Username     string          `json:"username"`
	Password     string          `json:"password"`
	ModulusKey   string          `json:"modulus_pubkey"`
	SRPSaltBytes int             `json:"srp_salt_bytes"`
	LabelCounts  map[string]int  `json:"label_counts"`
	Messages     []seededMessage `json:"messages"`
	// Set only with -request-log: the file the server appends every
	// lane-phase call to, so the test can show reading mutated nothing.
	RequestLog string `json:"request_log,omitempty"`
}

// The user labels and folders the driver creates.
type labelIDs struct {
	work     string
	projects string
	travel   string
	receipts string
}

// One message to seed. `star` is applied after the import, because the
// importer counts a folder and Starred as two exclusive labels.
type seed struct {
	subject string
	labels  []string
	unread  bool
	star    bool
}

func main() {
	if err := run(); err != nil {
		log.Fatal(err)
	}
}

func run() error {
	username := flag.String("username", defaultUsername, "synthetic user name")
	password := flag.String("password", defaultPassword, "synthetic password")
	requestLog := flag.String("request-log", "", "append every server call after the JSON line to this file")
	flag.Parse()

	s := server.New(server.WithTLS(true))
	defer s.Close()

	userID, _, err := s.CreateUser(*username, []byte(*password))
	if err != nil {
		return fmt.Errorf("create the synthetic user: %w", err)
	}

	labels, err := seedLabels(s, userID)
	if err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	session, err := signIn(ctx, s.GetHostURL(), *username, *password)
	if err != nil {
		return err
	}
	defer session.client.Close()

	seeded, counts, err := seedMessages(
		ctx,
		session.client,
		session.addressID,
		session.keyRings,
		*username,
		labels,
	)
	if err != nil {
		return err
	}

	// The development server's own TLS identity is httptest's self-signed
	// CA:TRUE certificate, which rustls refuses as an end entity
	// (`CaUsedAsEndEntity`). The lane terminates TLS itself with an
	// ephemeral CA:FALSE certificate for 127.0.0.1 and proxies to the
	// server's own TLS, so Sweetbriar's verifier sees a normal leaf.
	cert, err := generateCert()
	if err != nil {
		return err
	}
	frontend, err := startFrontend(s.GetHostURL(), cert)
	if err != nil {
		return err
	}
	defer frontend.Close()

	line := report{
		BaseURL:      "https://" + frontend.Addr().String(),
		CertPEM:      certificatePEM(cert),
		Username:     *username,
		Password:     *password,
		ModulusKey:   srp.GetModulusKey(),
		SRPSaltBytes: session.srpSaltBytes,
		LabelCounts:  counts,
		Messages:     seeded,
	}

	// The watcher starts before the line is printed, so no lane request
	// is missed.
	if *requestLog != "" {
		file, err := os.Create(*requestLog)
		if err != nil {
			return fmt.Errorf("create the request log: %w", err)
		}
		defer file.Close()
		s.AddCallWatcher(func(call server.Call) {
			fmt.Fprintf(file, "%s\t%s\n", call.Method, call.URL.Path)
		})
		line.RequestLog = *requestLog
	}

	if err := json.NewEncoder(os.Stdout).Encode(line); err != nil {
		return fmt.Errorf("write the lane line: %w", err)
	}

	return block()
}

func seedLabels(s *server.Server, userID string) (labelIDs, error) {
	work, err := s.CreateLabel(userID, "Work", "", proton.LabelTypeFolder)
	if err != nil {
		return labelIDs{}, fmt.Errorf("create the Work folder: %w", err)
	}
	projects, err := s.CreateLabel(userID, "Projects", work, proton.LabelTypeFolder)
	if err != nil {
		return labelIDs{}, fmt.Errorf("create the Work/Projects folder: %w", err)
	}
	travel, err := s.CreateLabel(userID, "Travel", "", proton.LabelTypeLabel)
	if err != nil {
		return labelIDs{}, fmt.Errorf("create the Travel label: %w", err)
	}
	receipts, err := s.CreateLabel(userID, "Receipts", "", proton.LabelTypeLabel)
	if err != nil {
		return labelIDs{}, fmt.Errorf("create the Receipts label: %w", err)
	}
	return labelIDs{work: work, projects: projects, travel: travel, receipts: receipts}, nil
}

// The signed-in Go client, its unlocked address keys, and the server's
// SRP salt length (production's is 10 bytes).
type session struct {
	client       *proton.Client
	addressID    string
	keyRings     map[string]*crypto.KeyRing
	srpSaltBytes int
}

// Signs in as the synthetic user and unlocks its address keys, the same
// path the Go client's own tests use.
func signIn(
	ctx context.Context,
	host, username, password string,
) (session, error) {
	manager := proton.New(
		proton.WithHostURL(host),
		// The development server's httptest certificate is self-signed;
		// the lane's Rust side pins it from the JSON line instead.
		proton.WithTransport(proton.InsecureTransport()),
	)
	client, _, err := manager.NewClientWithLogin(ctx, username, []byte(password))
	if err != nil {
		manager.Close()
		return session{}, fmt.Errorf("sign in as %s: %w", username, err)
	}

	info, err := manager.AuthInfo(ctx, proton.AuthInfoReq{Username: username})
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("read the SRP auth info: %w", err)
	}
	salt, err := base64.StdEncoding.DecodeString(info.Salt)
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("decode the SRP salt: %w", err)
	}

	user, err := client.GetUser(ctx)
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("get the synthetic user: %w", err)
	}
	addresses, err := client.GetAddresses(ctx)
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("get the synthetic addresses: %w", err)
	}
	salts, err := client.GetSalts(ctx)
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("get the synthetic key salts: %w", err)
	}
	keyPass, err := salts.SaltForKey([]byte(password), user.Keys.Primary().ID)
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("derive the key passphrase: %w", err)
	}
	_, keyRings, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{})
	if err != nil {
		client.Close()
		return session{}, fmt.Errorf("unlock the synthetic keys: %w", err)
	}
	if len(addresses) == 0 {
		client.Close()
		return session{}, errors.New("the synthetic user has no address")
	}
	return session{
		client:       client,
		addressID:    addresses[0].ID,
		keyRings:     keyRings,
		srpSaltBytes: len(salt),
	}, nil
}

func seedMessages(
	ctx context.Context,
	client *proton.Client,
	addressID string,
	keyRings map[string]*crypto.KeyRing,
	username string,
	labels labelIDs,
) ([]seededMessage, map[string]int, error) {
	keyRing := keyRings[addressID]
	if keyRing == nil {
		return nil, nil, errors.New("the synthetic address key did not unlock")
	}

	names := map[string]string{
		proton.InboxLabel:   "Inbox",
		proton.StarredLabel: "Starred",
		labels.work:         "Work",
		labels.projects:     "Work/Projects",
		labels.travel:       "Travel",
		labels.receipts:     "Receipts",
	}
	seeds := []seed{
		{subject: "lane inbox travel one", labels: []string{proton.InboxLabel, labels.travel}, unread: true},
		{subject: "lane inbox travel two", labels: []string{proton.InboxLabel, labels.travel}, unread: false},
		{subject: "lane inbox receipts", labels: []string{proton.InboxLabel, labels.receipts}, unread: true},
		{
			subject: "lane work travel receipts starred",
			labels:  []string{labels.work, labels.travel, labels.receipts},
			unread:  true,
			star:    true,
		},
		{subject: "lane projects travel", labels: []string{labels.projects, labels.travel}, unread: true},
		{subject: "lane starred travel", labels: []string{proton.StarredLabel, labels.travel}, unread: false},
	}

	seeded := make([]seededMessage, 0, len(seeds))
	counts := make(map[string]int)
	for _, item := range seeds {
		id, err := importOne(ctx, client, keyRing, addressID, item.labels, item.unread, item.subject, username)
		if err != nil {
			return nil, nil, err
		}

		effective := append([]string{}, item.labels...)
		if item.star {
			if err := client.LabelMessages(ctx, []string{id}, proton.StarredLabel); err != nil {
				return nil, nil, fmt.Errorf("star %q: %w", item.subject, err)
			}
			effective = append(effective, proton.StarredLabel)
		}

		labelNames := make([]string, 0, len(effective))
		for _, labelID := range effective {
			name := names[labelID]
			labelNames = append(labelNames, name)
			counts[name]++
		}
		seeded = append(seeded, seededMessage{
			ProviderKey: id,
			Subject:     item.subject,
			LabelNames:  labelNames,
			Unread:      item.unread,
		})
	}
	return seeded, counts, nil
}

func importOne(
	ctx context.Context,
	client *proton.Client,
	keyRing *crypto.KeyRing,
	addressID string,
	labelIDs []string,
	unread bool,
	subject, username string,
) (string, error) {
	page, err := client.ImportMessages(ctx, keyRing, 1, 1, proton.ImportReq{
		Metadata: proton.ImportMetadata{
			AddressID: addressID,
			LabelIDs:  labelIDs,
			Flags:     proton.MessageFlagReceived,
			Unread:    proton.Bool(unread),
		},
		Message: literal(subject, username),
	})
	if err != nil {
		return "", fmt.Errorf("import %q: %w", subject, err)
	}

	var messageIDs []string
	for {
		result, err := page.Next(ctx)
		if errors.Is(err, stream.End) {
			break
		}
		if err != nil {
			return "", fmt.Errorf("import %q: %w", subject, err)
		}
		if result.Code != proton.SuccessCode {
			return "", fmt.Errorf("import %q: code %d: %s", subject, result.Code, result.Message)
		}
		messageIDs = append(messageIDs, result.MessageID)
	}
	if len(messageIDs) != 1 {
		return "", fmt.Errorf("import %q: %d results, want one", subject, len(messageIDs))
	}
	return messageIDs[0], nil
}

func literal(subject, username string) []byte {
	return []byte(fmt.Sprintf(
		"From: Sender <sender@example.test>\r\n"+
			"To: %s@proton.local\r\n"+
			"Subject: %s\r\n"+
			"Date: Mon, 02 Jan 2006 15:04:05 -0700\r\n"+
			"\r\n"+
			"Synthetic lane message: %s.\r\n",
		username, subject, subject,
	))
}

// An ephemeral CA:FALSE leaf for 127.0.0.1, so Sweetbriar's rustls
// verifier accepts the lane endpoint (the server's own httptest
// certificate is a CA and would be refused as an end entity).
func generateCert() (tls.Certificate, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return tls.Certificate{}, fmt.Errorf("generate the lane key: %w", err)
	}
	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return tls.Certificate{}, fmt.Errorf("generate the lane serial: %w", err)
	}
	template := x509.Certificate{
		SerialNumber:          serial,
		Subject:               pkix.Name{CommonName: "sweetbriar-go-proton-api-lane"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(7 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
		IsCA:                  false,
		IPAddresses:           []net.IP{net.ParseIP("127.0.0.1")},
	}
	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		return tls.Certificate{}, fmt.Errorf("create the lane certificate: %w", err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, nil
}

func certificatePEM(cert tls.Certificate) string {
	return string(pem.EncodeToMemory(&pem.Block{
		Type:  "CERTIFICATE",
		Bytes: cert.Certificate[0],
	}))
}

// An HTTPS listener in front of the development server, so the lane's
// Rust client sees the generated leaf rather than httptest's CA.
func startFrontend(upstream string, cert tls.Certificate) (net.Listener, error) {
	target, err := url.Parse(upstream)
	if err != nil {
		return nil, fmt.Errorf("parse the development server URL: %w", err)
	}
	proxy := httputil.NewSingleHostReverseProxy(target)
	proxy.Transport = &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}}

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("listen for the lane frontend: %w", err)
	}
	tlsListener := tls.NewListener(listener, &tls.Config{
		Certificates: []tls.Certificate{cert},
		NextProtos:   []string{"http/1.1"},
	})
	server := &http.Server{Handler: proxy}
	go func() {
		_ = server.Serve(tlsListener)
	}()
	return listener, nil
}

// Blocks until the process is killed or stdin closes.
func block() error {
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGINT, syscall.SIGTERM)
	stdin := make(chan struct{})
	go func() {
		_, _ = io.Copy(io.Discard, os.Stdin)
		close(stdin)
	}()
	select {
	case <-signals:
	case <-stdin:
	}
	return nil
}
