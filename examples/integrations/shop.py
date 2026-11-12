# A complete shop integration in one file. Needs Python 3.8 or later, no dependencies.
#   GET  /buy      creates an invoice and sends the customer to the payment page
#   POST /webhook  receives payment updates from xmrgate
#
# Run:  XMRGATE_URL=https://pay.example.com XMRGATE_KEY=... SHOP_URL=http://localhost:8000 \
#       python3 shop.py

import hashlib
import hmac
import json
import os
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse

GATEWAY = os.environ["XMRGATE_URL"].rstrip("/")
KEY = os.environ["XMRGATE_KEY"]
SHOP = os.environ["SHOP_URL"].rstrip("/")


class Shop(BaseHTTPRequestHandler):
    def reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/buy":
            request = urllib.request.Request(
                f"{GATEWAY}/api/v1/invoices",
                method="POST",
                headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"},
                data=json.dumps({
                    "amount": "19.99",
                    "currency": "EUR",
                    "order_ref": f"order-{int(time.time())}",
                    "callback_url": f"{SHOP}/webhook",
                    "redirect_url": f"{SHOP}/thanks",
                }).encode(),
            )
            try:
                with urllib.request.urlopen(request, timeout=20) as answer:
                    invoice = json.load(answer)
            except OSError:
                self.reply(502, b"Monero payments are unavailable right now.")
                return
            self.reply(303, headers={"Location": invoice["checkout_url"]})
            return
        page = b"Thank you!" if path == "/thanks" else b'<a href="/buy">Buy for 19.99 EUR</a>'
        self.reply(200, page, {"Content-Type": "text/html"})

    def do_POST(self):
        if urlparse(self.path).path != "/webhook":
            self.reply(404)
            return
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        expected = hmac.new(KEY.encode(), body, hashlib.sha256).hexdigest()
        given = self.headers.get("X-Xmrgate-Signature", "")
        # Only xmrgate knows the key, so a valid signature proves the update is genuine.
        if not hmac.compare_digest(expected, given):
            self.reply(401)
            return
        invoice = json.loads(body)["invoice"]
        if invoice["status"] == "confirmed":
            # Mark the order as paid here.
            print(f"PAID {invoice['order_ref']} {invoice['received_xmr']} XMR", flush=True)
        else:
            print(f"UPDATE {invoice['order_ref']} {invoice['status']}", flush=True)
        self.reply(200)

    def log_message(self, *args):
        pass


HTTPServer(("127.0.0.1", urlparse(SHOP).port or 80), Shop).serve_forever()
