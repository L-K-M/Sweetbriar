"""Shared data generation + timing helpers for Sweetbriar store benchmarks."""
import json, os, random, struct, sys, time

_e = os.environ.get
ROWS = int(_e("BENCH_ROWS", 1_000_000))          # total messages
BIG_MBOX = 1              # mailbox id holding 500k rows
BIG_MBOX_ROWS = int(_e("BENCH_BIG_MBOX_ROWS", 500_000))
N_MAILBOXES = 40          # mailboxes for the remaining rows
SYNC_BATCHES = int(_e("BENCH_SYNC_BATCHES", 400))
SYNC_BATCH_ROWS = int(_e("BENCH_SYNC_BATCH_ROWS", 250))
SINGLE_COMMITS = int(_e("BENCH_SINGLE_COMMITS", 2_000))
POINT_LOOKUPS = int(_e("BENCH_POINT_LOOKUPS", 20_000))
KEYSET_PAGES = 40
KEYSET_PAGE_SIZE = 50
MARK_READ = int(_e("BENCH_MARK_READ", 10_000))
FTS_DOCS = int(_e("BENCH_FTS_DOCS", 200_000))
FTS_QUERIES = 200

DATA = os.path.join(os.path.dirname(__file__), "..", "data")
RESULTS = os.path.join(os.path.dirname(__file__), "..", "results")
os.makedirs(DATA, exist_ok=True)
os.makedirs(RESULTS, exist_ok=True)

WORDS = ("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu "
         "nu xi omicron pi rho sigma tau upsilon phi chi psi omega invoice "
         "meeting agenda minutes report deadline budget forecast release "
         "milestone deploy rollback urgent review approval draft proposal "
         "contract invoice payment receipt newsletter digest summary update "
         "security patch outage incident postmortem retro planning sprint "
         "customer vendor partner renewal quote pricing discount").split()


def word_stream(n, rng):
    return " ".join(rng.choice(WORDS) for _ in range(n))


def gen_rows(n, rng, base_id=0):
    """Yield tuples (id, mailbox, uid, uidvalidity, date, flags, size, thread, sender, subject)."""
    rows = []
    for i in range(n):
        mid = base_id + i
        mbox = BIG_MBOX if i < BIG_MBOX_ROWS else 2 + (i % N_MAILBOXES)
        uid = 100000 + i
        date = 1_700_000_000 - rng.randrange(0, 500_000_000)
        flags = rng.randrange(0, 8)
        size = rng.randrange(500, 200_000)
        thread = mid // rng.randrange(1, 5)
        sender = f"user{rng.randrange(1, 9000)}@example.com"
        subj = word_stream(rng.randrange(3, 10), rng)
        rows.append((mid, mbox, uid, 26_000 + mbox, date, flags, size,
                     thread, sender, subj))
    return rows


def fts_docs(n, rng):
    for i in range(n):
        yield (i, word_stream(rng.randrange(3, 10), rng),
               word_stream(rng.randrange(40, 140), rng))


def timed(fn):
    t0 = time.perf_counter()
    r = fn()
    return r, time.perf_counter() - t0


def report(engine, results):
    out = os.path.join(RESULTS, f"{engine}.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
    print(f"[{engine}] wrote {out}", flush=True)


def dir_size(path):
    """Total size in bytes of a file or directory tree."""
    if os.path.isfile(path):
        return os.path.getsize(path)
    total = 0
    for root, _, files in os.walk(path):
        for fn in files:
            total += os.path.getsize(os.path.join(root, fn))
    return total


def pack_msg(row):
    """msg row -> bytes for KV engines: fixed part + sender + subject."""
    mid, mbox, uid, uvv, date, flags, size, thread, sender, subj = row
    fixed = struct.pack("<QQIQQIIQ", mid, mbox, uid, uvv, date, flags, size, thread)
    return fixed + sender.encode() + b"|" + subj.encode()


def idx_key(mbox, date, mid):
    """mailbox index key: sorts by (mailbox, date DESC, id DESC)."""
    return struct.pack(">IQQ", mbox, (1 << 64) - 1 - date, (1 << 64) - 1 - mid)


def msg_key(mid):
    return b"m" + struct.pack(">Q", mid)
