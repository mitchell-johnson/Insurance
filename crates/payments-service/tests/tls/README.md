Test certificates for the fake bank's mutual TLS. Never used outside tests.

- `ca.pem`: the test bank's certificate authority (its key was discarded).
- `server.pem`, `server.key`: the fake bank, for `localhost` and `127.0.0.1`.
- `client.pem`, `client.key`: the insurer's client certificate, issued by the CA.
- `stranger.pem`, `stranger.key`: a self-signed client certificate the bank never issued.

All are valid for 100 years from 13 November 2026.
