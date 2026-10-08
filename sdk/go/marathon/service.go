// Package marathon is the Go helper for writing Marathon services.
//
// A service is a Unix socket speaking newline-delimited JSON. You declare
// topics (state that widgets subscribe to) and methods (actions they call);
// the helper handles the protocol, multiple clients and socket cleanup.
//
//	svc := marathon.New("power")
//	battery := svc.Topic("battery", map[string]any{"percent": 100})
//	svc.Method("set_profile", func(args json.RawMessage) (any, error) { ... })
//	go func() { for { battery.Publish(read()); time.Sleep(2 * time.Second) } }()
//	log.Fatal(svc.Serve())
package marathon

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"reflect"
	"sort"
	"sync"
	"syscall"
)

type MethodFunc func(args json.RawMessage) (any, error)

type Service struct {
	name    string
	mu      sync.Mutex
	topics  map[string]*Topic
	methods map[string]MethodFunc
}

type Topic struct {
	svc   *Service
	path  string
	value any
	subs  map[*client]map[uint64]struct{}
}

type client struct {
	enc *json.Encoder
	mu  sync.Mutex
}

type request struct {
	ID     uint64          `json:"id"`
	Op     string          `json:"op"`
	Path   string          `json:"path,omitempty"`
	Method string          `json:"method,omitempty"`
	Args   json.RawMessage `json:"args,omitempty"`
	Sub    uint64          `json:"sub,omitempty"`
}

type response struct {
	ID     uint64 `json:"id"`
	Result any    `json:"result,omitempty"`
	Error  string `json:"error,omitempty"`
	Event  any    `json:"event,omitempty"`
}

func New(name string) *Service {
	return &Service{name: name, topics: map[string]*Topic{}, methods: map[string]MethodFunc{}}
}

// Topic declares a subscribable path with its initial value.
func (s *Service) Topic(path string, initial any) *Topic {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := &Topic{svc: s, path: path, value: initial, subs: map[*client]map[uint64]struct{}{}}
	s.topics[path] = t
	return t
}

func (s *Service) Method(name string, fn MethodFunc) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.methods[name] = fn
}

// Publish sets the topic's value and notifies subscribers if it changed.
func (t *Topic) Publish(v any) {
	t.svc.mu.Lock()
	if reflect.DeepEqual(t.value, v) {
		t.svc.mu.Unlock()
		return
	}
	t.value = v
	type target struct {
		c  *client
		id uint64
	}
	var targets []target
	for c, ids := range t.subs {
		for id := range ids {
			targets = append(targets, target{c, id})
		}
	}
	t.svc.mu.Unlock()
	for _, tg := range targets {
		tg.c.send(response{ID: tg.id, Event: v})
	}
}

func (t *Topic) Value() any {
	t.svc.mu.Lock()
	defer t.svc.mu.Unlock()
	return t.value
}

func (c *client) send(r response) {
	c.mu.Lock()
	defer c.mu.Unlock()
	_ = c.enc.Encode(r)
}

// SocketPath is $MARATHON_SERVICES_DIR/<name>.sock, falling back to
// $XDG_RUNTIME_DIR/marathon/services/<name>.sock.
func SocketPath(name string) string {
	dir := os.Getenv("MARATHON_SERVICES_DIR")
	if dir == "" {
		runtime := os.Getenv("XDG_RUNTIME_DIR")
		if runtime == "" {
			runtime = os.TempDir()
		}
		dir = filepath.Join(runtime, "marathon", "services")
	}
	return filepath.Join(dir, name+".sock")
}

// Serve listens until SIGINT/SIGTERM, then removes the socket.
func (s *Service) Serve() error {
	path := SocketPath(s.name)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	_ = os.Remove(path) // stale socket from a previous run
	ln, err := net.Listen("unix", path)
	if err != nil {
		return err
	}
	_ = os.Chmod(path, 0o600) // only this user may connect
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-sig
		ln.Close()
	}()
	fmt.Fprintf(os.Stderr, "%s: listening on %s\n", s.name, path)
	for {
		conn, err := ln.Accept()
		if err != nil {
			_ = os.Remove(path)
			if errors.Is(err, net.ErrClosed) {
				return nil
			}
			return err
		}
		go s.handle(conn)
	}
}

func (s *Service) handle(conn net.Conn) {
	defer conn.Close()
	c := &client{enc: json.NewEncoder(conn)}
	defer s.drop(c)
	sc := bufio.NewScanner(conn)
	sc.Buffer(make([]byte, 64*1024), 1<<20)
	for sc.Scan() {
		var req request
		if err := json.Unmarshal(sc.Bytes(), &req); err != nil {
			c.send(response{Error: "bad request: " + err.Error()})
			continue
		}
		s.dispatch(c, req)
	}
}

func (s *Service) dispatch(c *client, req request) {
	switch req.Op {
	case "get", "subscribe":
		s.mu.Lock()
		t, ok := s.topics[req.Path]
		var v any
		if ok {
			v = t.value
			if req.Op == "subscribe" {
				if t.subs[c] == nil {
					t.subs[c] = map[uint64]struct{}{}
				}
				t.subs[c][req.ID] = struct{}{}
			}
		}
		s.mu.Unlock()
		switch {
		case !ok:
			c.send(response{ID: req.ID, Error: "unknown path " + req.Path})
		case req.Op == "get":
			c.send(response{ID: req.ID, Result: v})
		default:
			c.send(response{ID: req.ID, Event: v}) // current value first
		}
	case "unsubscribe":
		s.mu.Lock()
		for _, t := range s.topics {
			delete(t.subs[c], req.Sub)
		}
		s.mu.Unlock()
		c.send(response{ID: req.ID})
	case "call":
		s.mu.Lock()
		fn, ok := s.methods[req.Method]
		s.mu.Unlock()
		if !ok {
			c.send(response{ID: req.ID, Error: "unknown method " + req.Method})
			return
		}
		res, err := fn(req.Args)
		if err != nil {
			c.send(response{ID: req.ID, Error: err.Error()})
			return
		}
		c.send(response{ID: req.ID, Result: res})
	case "describe":
		s.mu.Lock()
		d := map[string]any{"name": s.name, "paths": keys(s.topics), "methods": keys(s.methods)}
		s.mu.Unlock()
		c.send(response{ID: req.ID, Result: d})
	default:
		c.send(response{ID: req.ID, Error: "unknown op " + req.Op})
	}
}

func (s *Service) drop(c *client) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, t := range s.topics {
		delete(t.subs, c)
	}
}

func keys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
