# Root certificates for the TLS spike

`roots.pem` is the small set of public root CAs that the `tls-spike` binary trusts. It is not a general trust store.
It covers the common public chains (Let's Encrypt, DigiCert, Google Trust Services, Sectigo/USERTrust, Microsoft,
Amazon, SSL.com, GlobalSign) so that most HTTPS endpoints verify. Each block starts with a plain-text name line,
which the PEM parser skips.

The certificates are copied unchanged from the Mozilla root store (Debian/Ubuntu `ca-certificates`). To rebuild the
file, for example after a root rotates:

```sh
for n in ISRG_Root_X1 ISRG_Root_X2 ...; do cat /usr/share/ca-certificates/mozilla/$n.crt; done > roots.pem
```

Roots are public data, so nothing in this folder is secret. Private CAs (a self-hosted Gitea) are not handled yet.
