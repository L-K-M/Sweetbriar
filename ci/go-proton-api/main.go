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
	"mime"
	"net"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
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
	defaultUsername     = "laneuser"
	defaultPassword     = "lane-password"
	defaultSendUsername = "lanesender"
	defaultPeerUsername = "lanepeer"
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
	// The send suite's synthetic sender and internal recipient (M4.3g).
	// Both share the lane password; the recipient's inbox is the read-back.
	SendUsername string `json:"send_username"`
	SendEmail    string `json:"send_email"`
	PeerEmail    string `json:"peer_email"`
	// Set only with -send-log: one JSON record per send-suite call, with
	// its HTTP status, Proton code/error and whether the recipient's
	// inbox grew, so the lane can tell a refusal that sent nothing.
	SendLog string `json:"send_log,omitempty"`
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
	sendUsername := flag.String("send-username", defaultSendUsername, "synthetic send-suite sender user name")
	peerUsername := flag.String("peer-username", defaultPeerUsername, "synthetic send-suite recipient user name")
	requestLog := flag.String("request-log", "", "append every server call after the JSON line to this file")
	sendLogPath := flag.String("send-log", "", "append one JSON record per send-suite call to this file")
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

	// The send suite's sender and internal recipient: a second and
	// third synthetic account, so the send suite never mutates the
	// read-only suite's account. The driver reads the recipient's inbox
	// back after every send-suite call (the lane's read-back).
	if _, _, err := s.CreateUser(*sendUsername, []byte(*password)); err != nil {
		return fmt.Errorf("create the send-suite sender: %w", err)
	}
	if _, _, err := s.CreateUser(*peerUsername, []byte(*password)); err != nil {
		return fmt.Errorf("create the send-suite recipient: %w", err)
	}
	peerClient, err := signInClient(ctx, s.GetHostURL(), *peerUsername, *password)
	if err != nil {
		return fmt.Errorf("sign in as the send-suite recipient: %w", err)
	}
	defer peerClient.Close()

	sends, err := newSendLog(*sendLogPath, peerClient)
	if err != nil {
		return err
	}
	defer sends.close()

	// The development server's own TLS identity is httptest's self-signed
	// CA:TRUE certificate, which rustls refuses as an end entity
	// (`CaUsedAsEndEntity`). The lane terminates TLS itself with an
	// ephemeral CA:FALSE certificate for 127.0.0.1 and proxies to the
	// server's own TLS, so Sweetbriar's verifier sees a normal leaf.
	cert, err := generateCert()
	if err != nil {
		return err
	}
	frontend, err := startFrontend(s.GetHostURL(), cert, sends)
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
		SendUsername: *sendUsername,
		SendEmail:    *sendUsername + "@" + s.GetDomain(),
		PeerEmail:    *peerUsername + "@" + s.GetDomain(),
		SendLog:      *sendLogPath,
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

// Signs in a synthetic user with the Go client, without unlocking its
// keys: the send suite's recipient read-back only reads metadata.
func signInClient(
	ctx context.Context,
	host, username, password string,
) (*proton.Client, error) {
	manager := proton.New(
		proton.WithHostURL(host),
		proton.WithTransport(proton.InsecureTransport()),
	)
	client, _, err := manager.NewClientWithLogin(ctx, username, []byte(password))
	if err != nil {
		manager.Close()
		return nil, fmt.Errorf("sign in as %s: %w", username, err)
	}
	return client, nil
}

// One send-suite call's record: the HTTP status and Proton's code/error
// the proxy saw, and whether the recipient's inbox grew as a result.
// The lane reads these to tell a refusal that sent nothing from one
// that delivered (M4.3g).

type sendRecord struct {
	Sequence int64  `json:"sequence"`
	Method   string `json:"method"`
	Path     string `json:"path"`
	Status   int    `json:"status"`
	Code     *int64 `json:"code"`
	Error    string `json:"error"`
	Inbox    int    `json:"inbox"`
	Sent     bool   `json:"sent"`
	ReadBack string `json:"read_back"`
}

// The append-only send log. The proxy calls record after each
// send-suite call; the recipient's inbox is read directly from the
// development server, so the record holds the read-back the lane
// asserts on.

type sendLog struct {
	mu       sync.Mutex
	file     *os.File
	sequence int64
	inbox    int
	peer     *proton.Client
}

func newSendLog(path string, peer *proton.Client) (*sendLog, error) {
	if path == "" {
		return nil, nil
	}
	file, err := os.Create(path)
	if err != nil {
		return nil, fmt.Errorf("create the send log: %w", err)
	}
	log := &sendLog{file: file, peer: peer}
	log.inbox = log.readInbox()
	if log.inbox < 0 {
		_ = file.Close()
		return nil, fmt.Errorf("read the recipient's inbox before the send suite")
	}
	return log, nil
}

func (l *sendLog) close() {
	if l != nil && l.file != nil {
		_ = l.file.Close()
	}
}

// readInbox counts the recipient's Inbox messages. -1 means the read
// failed; the caller keeps the previous count and says so.
func (l *sendLog) readInbox() int {
	if l == nil || l.peer == nil {
		return 0
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	metadata, err := l.peer.GetMessageMetadata(ctx, proton.MessageFilter{LabelID: proton.InboxLabel})
	if err != nil {
		return -1
	}
	return len(metadata)
}

func (l *sendLog) record(request *http.Request, status int, body []byte) {
	if l == nil || !isSendPath(request.Method, request.URL.Path) {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()

	code, text := responseCodeError(body)
	inbox := l.readInbox()
	readBack := "ok"
	if inbox < 0 {
		readBack = "failed"
		inbox = l.inbox
	}
	sent := inbox > l.inbox
	l.inbox = inbox
	l.sequence++
	_ = json.NewEncoder(l.file).Encode(sendRecord{
		Sequence: l.sequence,
		Method:   request.Method,
		Path:     request.URL.Path,
		Status:   status,
		Code:     code,
		Error:    text,
		Inbox:    inbox,
		Sent:     sent,
		ReadBack: readBack,
	})
}

// The calls the send suite drives: the recipient key lookup, draft
// create and update, attachment upload and send.
func isSendPath(method, path string) bool {
	if method == http.MethodGet && path == "/core/v4/keys" {
		return true
	}
	if method == http.MethodPost && (path == "/mail/v4/messages" || path == "/mail/v4/attachments") {
		return true
	}
	return strings.HasPrefix(path, "/mail/v4/messages/") &&
		(method == http.MethodPost || method == http.MethodPut)
}

// responseCodeError reads Proton's `Code` and `Error` from a reply. A
// bare status (the development server's 422s carry no body) has
// neither; the lane records that honestly.
func responseCodeError(body []byte) (*int64, string) {
	var response struct {
		Code  *int64 `json:"Code"`
		Error string `json:"Error"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return nil, ""
	}
	return response.Code, response.Error
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
func startFrontend(upstream string, cert tls.Certificate, sends *sendLog) (net.Listener, error) {
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
	server := &http.Server{Handler: laneProxy(proxy, sends)}
	go func() {
		_ = server.Serve(tlsListener)
	}()
	return listener, nil
}

// laneProxy papers over the response differences that block
// Sweetbriar's client without touching the pinned server: production
// puts `Code` on every success, `Version` on key objects and empty
// address arrays where the server sends nulls. Only 2xx JSON object
// responses are rewritten; raw bodies such as attachment packets pass
// through untouched.
func laneProxy(next http.Handler, sends *sendLog) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		recorder := httptest.NewRecorder()
		next.ServeHTTP(recorder, r)

		header := recorder.Header()
		body := recorder.Body.Bytes()
		if recorder.Code >= 200 && recorder.Code < 300 &&
			isJSON(header.Get("Content-Type")) &&
			header.Get("Content-Encoding") == "" && len(body) > 0 {
			if patched, ok := rewriteLaneResponse(r.URL.Path, body); ok {
				body = patched
				header.Set("Content-Length", strconv.Itoa(len(body)))
			}
		}

		// Before the reply reaches the lane, so a record is always
		// there when its call returns.
		sends.record(r, recorder.Code, body)

		for name, values := range header {
			for _, value := range values {
				w.Header().Add(name, value)
			}
		}
		w.WriteHeader(recorder.Code)
		_, _ = w.Write(body)
	})
}

func isJSON(contentType string) bool {
	mediaType, _, err := mime.ParseMediaType(contentType)
	return err == nil && mediaType == "application/json"
}

func rewriteLaneResponse(path string, body []byte) ([]byte, bool) {
	var response map[string]any
	if err := json.Unmarshal(body, &response); err != nil || response == nil {
		return body, false
	}

	changed := false
	if _, ok := response["Code"]; !ok {
		response["Code"] = float64(proton.SuccessCode)
		changed = true
	}

	if isKeyPath(path) && addKeyVersions(response) {
		changed = true
	}
	if path == "/mail/v4/messages" && replaceNullLists(response) {
		changed = true
	}

	if !changed {
		return body, false
	}
	patched, err := json.Marshal(response)
	if err != nil {
		return body, false
	}
	return patched, true
}

// Key objects live under User.Keys, Address.Keys and Addresses[].Keys.
func isKeyPath(path string) bool {
	return path == "/core/v4/users" ||
		path == "/core/v4/addresses" ||
		strings.HasPrefix(path, "/core/v4/addresses/")
}

// replaceNullLists turns the `null` address lists the development
// server emits for absent CC, BCC or Reply-To into production's empty
// arrays: go-proton-api marshals nil slices as null, while Sweetbriar's
// metadata deserializer expects a sequence (spec/02 stores the raw
// object, and the in-process fake sends `[]`).
func replaceNullLists(response map[string]any) bool {
	changed := false
	fix := func(message map[string]any) {
		for _, field := range []string{"ToList", "CCList", "BCCList", "ReplyTos"} {
			if message[field] == nil {
				message[field] = []any{}
				changed = true
			}
		}
	}
	messages, ok := response["Messages"].([]any)
	if !ok {
		return false
	}
	for _, item := range messages {
		if message, ok := item.(map[string]any); ok {
			fix(message)
		}
	}
	return changed
}

// addKeyVersions adds production's key `Version` to every key object
// that lacks one. The development server's `proton.Key` model has no
// Version field at all, while proton-crypto-account's `LockedKey`
// requires one (it never reads it; the crypto checks the private key's
// own version). Production's captured key objects report 3, which is
// what this lane reports too.
func addKeyVersions(response map[string]any) bool {
	changed := false
	fix := func(object map[string]any) {
		keys, ok := object["Keys"].([]any)
		if !ok {
			return
		}
		for _, item := range keys {
			key, ok := item.(map[string]any)
			if !ok {
				continue
			}
			if _, present := key["Version"]; present {
				continue
			}
			key["Version"] = float64(3)
			changed = true
		}
	}

	if user, ok := response["User"].(map[string]any); ok {
		fix(user)
	}
	if address, ok := response["Address"].(map[string]any); ok {
		fix(address)
	}
	if addresses, ok := response["Addresses"].([]any); ok {
		for _, item := range addresses {
			if address, ok := item.(map[string]any); ok {
				fix(address)
			}
		}
	}
	return changed
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
