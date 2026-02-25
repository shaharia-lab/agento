.PHONY: build run-serve run-ask tidy lint test clean

BINARY := agents-platform

build:
	go build -o $(BINARY) .

run-serve: build
	./$(BINARY) serve

run-ask: build
	./$(BINARY) ask $(ARGS)

tidy:
	go mod tidy

lint:
	go vet ./...

test:
	go test ./...

clean:
	rm -f $(BINARY)
