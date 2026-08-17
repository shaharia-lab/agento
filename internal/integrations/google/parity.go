package google

import "golang.org/x/oauth2"

// This file is the seam `desktop/parity/google_parity_test.go` builds its
// cross-language vectors through, for the reason `telegram/parity.go` sets out:
// the vectors have to come from the **real** server, not from a restatement of
// it, and `desktop/parity` is a different package.
//
// Google needs a wider seam than the other five, and the reason is the whole
// difficulty of #313. The others build their requests with `http.NewRequest`, so
// a reader can see the bytes. Google calls the generated client libraries
// (`calendar/v3`, `gmail/v1`, `drive/v3`) over an `oauth2` transport, and what
// those put on the wire — the resolved URL, the sorted query, the `omitempty`
// body, the `multipart/related` framing, the `googleapi.Error` sentence — is in
// neither this repository nor the port. It has to be *recorded*, which means both
// the API base and the OAuth2 token endpoint must be redirectable at a local
// fake.
//
// Nothing below changes behavior or wording: `Start` remains the only way the app
// builds this server.

// SetEndpoints points the three generated clients at apiBase and every token
// refresh at tokenURL, returning a function that restores the previous values.
//
// Do not call outside tests. Both halves hand credentials to whatever host they
// name: the API base receives the access token in an `Authorization` header, and
// the token URL receives the **client secret and refresh token** — the durable
// credential, not the hour-long one. It is exported only because `desktop/parity`
// is a different package; the Rust port gates its equivalent behind
// `#[cfg(test)]`, so it does not exist in a shipped desktop binary at all.
//
// Not safe for concurrent use with a live integration — which is fine, because
// the only callers are tests that own the process.
func SetEndpoints(apiBase, tokenURL string) (restore func()) {
	previousAPI, previousOAuth := apiEndpoint, oauthEndpoint
	apiEndpoint = apiBase
	oauthEndpoint = oauth2.Endpoint{
		AuthURL:   tokenURL,
		TokenURL:  tokenURL,
		AuthStyle: oauth2.AuthStyleInParams,
	}
	return func() {
		apiEndpoint, oauthEndpoint = previousAPI, previousOAuth
	}
}
