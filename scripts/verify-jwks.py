#!/usr/bin/env python3
"""Verify an Agento API token from the outside (#405).

This is the by-hand half of the proof that `src-tauri/tests/jwks_external_verify.rs`
makes in CI. That test is honest about its limit: it is a separate binary, but
it is still Rust and still links the same `jsonwebtoken` over the same `ring`.
This script is a *different implementation* — PyJWT over `cryptography`, i.e.
OpenSSL — so a bug in how Agento signs, spells its `kid`, or encodes the public
key would have to be reproduced independently in two crypto stacks to escape
both.

It is not run by CI, which has no Python JWT library. Run it after changing
anything about the token format, the JWKS document, or the signing key:

    # with the desktop app running, and a token from Settings → Security
    scripts/verify-jwks.py --token "$(cat ~/.agento-desktop-dev/api-token)"

    # or against a release install, whose port is in the app's About screen
    scripts/verify-jwks.py --base http://127.0.0.1:PORT --token eyJ...

What it proves, in order:

1. `GET /.well-known/jwks.json` answers **with no credential**. If it did not,
   an external verifier could not bootstrap at all.
2. The document is RFC 8037-shaped: `kty: OKP`, `crv: Ed25519`, a 32-byte `x`.
3. The token verifies against the published key, with `iss` and `aud` enforced.
4. A token signed by a *different* Ed25519 key does **not** verify — so reading
   the public key does not let anyone mint one.

Requires: pyjwt, cryptography.
"""

import argparse
import base64
import json
import sys
import urllib.error
import urllib.request

try:
    import jwt
    from cryptography.hazmat.primitives.asymmetric import ed25519
except ImportError:  # pragma: no cover - a setup message, not a code path
    sys.exit("needs pyjwt and cryptography: pip install pyjwt cryptography")

ISSUER = "agento"
AUDIENCE = "agento-api"
JWKS_PATH = "/.well-known/jwks.json"


def b64url(raw: str) -> bytes:
    """Decode base64url without padding, which is how a JWK spells `x`."""
    return base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4))


def fetch_jwks(base: str) -> dict:
    url = base.rstrip("/") + JWKS_PATH
    # Deliberately no Authorization header: point 1 above is that this works
    # without one, and adding one here would hide a regression that made the
    # route guarded.
    with urllib.request.urlopen(url, timeout=10) as response:
        if response.status != 200:
            sys.exit(f"{url} answered {response.status}")
        return json.load(response)


def load_key(jwks: dict, kid: str) -> ed25519.Ed25519PublicKey:
    for jwk in jwks.get("keys", []):
        if jwk.get("kid") != kid:
            continue
        if jwk.get("kty") != "OKP" or jwk.get("crv") != "Ed25519":
            sys.exit(f"kid {kid} is not an Ed25519 OKP key: {jwk}")
        raw = b64url(jwk["x"])
        if len(raw) != 32:
            sys.exit(f"kid {kid} has a {len(raw)}-byte x; Ed25519 keys are 32")
        return ed25519.Ed25519PublicKey.from_public_bytes(raw)
    sys.exit(f"no key in the JWKS for kid {kid}; keys: "
             f"{[k.get('kid') for k in jwks.get('keys', [])]}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="http://127.0.0.1:8991",
                        help="Agento's origin (default: the dev proxy)")
    parser.add_argument("--token", required=True, help="a token to verify")
    args = parser.parse_args()

    try:
        jwks = fetch_jwks(args.base)
    except urllib.error.URLError as e:
        print(f"could not reach {args.base}{JWKS_PATH}: {e}", file=sys.stderr)
        print("Is the app running? A release build's port is in Settings → About.",
              file=sys.stderr)
        return 1

    print(f"✔ {args.base}{JWKS_PATH} served {len(jwks.get('keys', []))} key(s) "
          f"with no credential")

    header = jwt.get_unverified_header(args.token)
    if header.get("alg") != "EdDSA":
        sys.exit(f"expected alg EdDSA, got {header.get('alg')!r}")
    kid = header.get("kid")
    if not kid:
        sys.exit("the token carries no kid, so no key can be selected")

    key = load_key(jwks, kid)
    print(f"✔ header names kid={kid}, alg=EdDSA; the JWKS has a matching "
          f"Ed25519 key")

    claims = jwt.decode(args.token, key, algorithms=["EdDSA"],
                        issuer=ISSUER, audience=AUDIENCE)
    print(f"✔ signature and claims verify: sub={claims.get('sub')!r} "
          f"scope={claims.get('scope')!r} jti={claims.get('jti')!r}")

    # The other half: reading the public key must not let anyone mint one.
    impostor = ed25519.Ed25519PrivateKey.generate()
    forged = jwt.encode(
        {**claims, "scope": "write"}, impostor,
        algorithm="EdDSA", headers={"kid": kid},
    )
    try:
        jwt.decode(forged, key, algorithms=["EdDSA"],
                   issuer=ISSUER, audience=AUDIENCE)
    except jwt.InvalidSignatureError:
        print("✔ a token signed by another key is refused, even with the "
              "right kid")
    else:
        sys.exit("✘ a forged token verified — the published key is not the "
                 "one signing tokens")

    print("\nAll four properties hold. An external service can verify Agento's "
          "tokens offline\nand cannot mint them.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
