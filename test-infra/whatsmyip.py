"""Answer every GET with the caller's source address and nothing else, like api.ipify.org."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 8080


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = self.client_address[0].encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        print("%s %s" % (self.client_address[0], format % args), flush=True)


ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
