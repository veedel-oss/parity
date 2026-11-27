# Protocol

How the CLI and collect mode talk over the SSH session. It is implemented in `crates/parity-proto`, without dependencies; how the sessions are set up is in [architecture.md](architecture.md#connection-model).

## Framing

Every message is a frame: a 4-byte big-endian length, then that many bytes of payload. The frames run over the stdin and stdout of collect mode.

| Frames | At most |
| --- | --- |
| requests, as collect mode accepts them | 1 MiB |
| answers, as the CLI accepts them | 16 MiB |

## Encoding

The payload is one message, in a compact binary format written for Parity:

| Value | Encoding |
| --- | --- |
| integer | big-endian, fixed width |
| byte string, list | u32 length, then the bytes or items |
| option, enum | u8 tag, then the value |

Decoders check every length against the bytes left, so a forged length cannot make them allocate. Paths are byte strings, since Linux paths need not be UTF-8.

## Session

Both sides first send a hello: the magic `PRTY`, the protocol version as u32, and the program version. The encoding of the hello never changes, so two different versions can always detect the mismatch and report it. The protocol version rises with every incompatible change of a message.

Then the CLI sends requests, and collect mode answers each in order:

| Request | Answer |
| --- | --- |
| `Ping` | `Pong` with the same nonce |
| `DpkgStatus` | the raw `/var/lib/dpkg/status` in chunks, which the CLI parses |
| `Walk` | the metadata below the roots in batches of at most 1000 entries, errors per path, then an end marker |
| `ReadFile` | the content of one regular file in chunks of 256 KiB, at most 64 MiB, or an error |
| `End` | `Bye`, and the session ends |

A request that cannot be decoded ends the session. The fields of every message are in [message.rs](../crates/parity-proto/src/message.rs).
