package daemon

import (
	"bytes"
	"embed"
	"fmt"
	"text/template"
)

// templates holds the embedded launchd plist and systemd unit templates.
//
//go:embed templates
var templates embed.FS

// templateData is the substitution set shared by both unit templates.
type templateData struct {
	Label      string // launchd label (unused by the systemd template)
	BinaryPath string
	DataDir    string
	LogPath    string
	Port       int
	ExtraPath  string
}

// render executes the named embedded template against opts and returns the
// unit/plist file content.
func render(name string, opts Options) ([]byte, error) {
	tmpl, err := template.ParseFS(templates, "templates/"+name)
	if err != nil {
		return nil, fmt.Errorf("parsing embedded template %s: %w", name, err)
	}
	data := templateData{
		Label:      launchdLabel,
		BinaryPath: opts.BinaryPath,
		DataDir:    opts.DataDir,
		LogPath:    opts.LogPath,
		Port:       opts.Port,
		ExtraPath:  opts.ExtraPath,
	}
	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, data); err != nil {
		return nil, fmt.Errorf("rendering template %s: %w", name, err)
	}
	return buf.Bytes(), nil
}
