package daemon

import (
	"bytes"
	"embed"
	"encoding/xml"
	"fmt"
	"strings"
	"text/template"
)

// templates holds the embedded launchd plist and systemd unit templates.
//
//go:embed templates
var templates embed.FS

// templateData is the substitution set shared by both unit templates.
type templateData struct {
	Label           string // launchd label (unused by the systemd template)
	BinaryPath      string
	DataDir         string
	LogPath         string
	Port            int
	ExtraPath       string
	BindAddress     string
	ClaudeConfigDir string
}

// templateFuncs are the per-format escapers the templates pipe every
// substitution through. text/template performs no escaping of its own, so
// without these a space, quote, %, or XML metacharacter in a path produces a
// service definition the platform loader rejects or misparses. Each format
// gets its own rules — html/template is deliberately not used because it
// escapes for HTML, not XML plists or systemd units.
var templateFuncs = template.FuncMap{
	// systemdEnv/systemdArg escape one value for a double-quoted context:
	// \ and " are escaped, and % is doubled because systemd expands
	// specifiers (%i, %h, …) inside Environment= and ExecStart=. Callers
	// wrap the result in double quotes.
	"systemdEnv": escapeSystemdValue,
	"systemdArg": escapeSystemdValue,
	"xmlEscape":  escapeXML,
}

// systemdEscaper maps the characters that are unsafe inside a double-quoted
// systemd Environment= / ExecStart= value.
var systemdEscaper = strings.NewReplacer(
	`\`, `\\`,
	`"`, `\"`,
	"%", "%%",
)

// escapeSystemdValue escapes s for a double-quoted systemd value context.
// Accepting any keeps the templates uniform — a future type change (e.g.
// Port becoming a string) cannot reintroduce the unescaped-substitution bug.
func escapeSystemdValue(v any) string {
	return systemdEscaper.Replace(fmt.Sprint(v))
}

// escapeXML escapes a value for plist XML character data. xml.EscapeText is
// used rather than a hand-rolled replacer for correctness; it also escapes
// " and ' (as numeric entities), which is harmless inside <string> elements
// and keeps attribute contexts safe too.
func escapeXML(v any) string {
	var buf bytes.Buffer
	if err := xml.EscapeText(&buf, []byte(fmt.Sprint(v))); err != nil {
		// EscapeText only errors when the writer does; bytes.Buffer never does.
		panic(fmt.Sprintf("escaping XML: %v", err))
	}
	return buf.String()
}

// render executes the named embedded template against opts and returns the
// unit/plist file content.
func render(name string, opts Options) ([]byte, error) {
	// ParseFS names the parsed template by base name, so template.New must
	// match it (otherwise Execute renders an empty document).
	tmpl, err := template.New(name).Funcs(templateFuncs).ParseFS(templates, "templates/"+name)
	if err != nil {
		return nil, fmt.Errorf("parsing embedded template %s: %w", name, err)
	}
	data := templateData{
		Label:           launchdLabel,
		BinaryPath:      opts.BinaryPath,
		DataDir:         opts.DataDir,
		LogPath:         opts.LogPath,
		Port:            opts.Port,
		ExtraPath:       opts.ExtraPath,
		BindAddress:     opts.BindAddress,
		ClaudeConfigDir: opts.ClaudeConfigDir,
	}
	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, data); err != nil {
		return nil, fmt.Errorf("rendering template %s: %w", name, err)
	}
	return buf.Bytes(), nil
}
