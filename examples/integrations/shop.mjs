// A complete shop integration in one file. Needs Node 18 or later, no dependencies.
//   GET  /buy      creates an invoice and sends the customer to the payment page
//   POST /webhook  receives payment updates from xmrgate
//
// Run:  XMRGATE_URL=https://pay.example.com XMRGATE_KEY=... SHOP_URL=http://localhost:8000 \
//       node shop.mjs

import { createServer } from 'node:http';
import { createHmac, timingSafeEqual } from 'node:crypto';

const gateway = process.env.XMRGATE_URL.replace(/\/$/, '');
const key = process.env.XMRGATE_KEY;
const shop = process.env.SHOP_URL.replace(/\/$/, '');

createServer(async (request, response) => {
  const path = new URL(request.url, shop).pathname;

  if (path === '/buy') {
    const answer = await fetch(`${gateway}/api/v1/invoices`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({
        amount: '19.99',
        currency: 'EUR',
        order_ref: `order-${Date.now()}`,
        callback_url: `${shop}/webhook`,
        redirect_url: `${shop}/thanks`,
      }),
    });
    const invoice = await answer.json();
    if (!invoice.checkout_url) {
      response.writeHead(502).end('Monero payments are unavailable right now.');
      return;
    }
    response.writeHead(303, { Location: invoice.checkout_url }).end();
    return;
  }

  if (path === '/webhook') {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    const expected = Buffer.from(createHmac('sha256', key).update(body).digest('hex'));
    const given = Buffer.from(String(request.headers['x-xmrgate-signature'] || ''));
    // Only xmrgate knows the key, so a valid signature proves the update is genuine.
    if (given.length !== expected.length || !timingSafeEqual(given, expected)) {
      response.writeHead(401).end();
      return;
    }
    const { invoice } = JSON.parse(body);
    if (invoice.status === 'confirmed') {
      // Mark the order as paid here.
      console.log(`PAID ${invoice.order_ref} ${invoice.received_xmr} XMR`);
    } else {
      console.log(`UPDATE ${invoice.order_ref} ${invoice.status}`);
    }
    response.writeHead(200).end();
    return;
  }

  response
    .writeHead(200, { 'Content-Type': 'text/html' })
    .end(path === '/thanks' ? 'Thank you!' : '<a href="/buy">Buy for 19.99 EUR</a>');
}).listen(new URL(shop).port || 80);
