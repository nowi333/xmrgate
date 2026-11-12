<?php
// A complete shop integration in one file.
//   GET  /buy      creates an invoice and sends the customer to the payment page
//   POST /webhook  receives payment updates from xmrgate
//
// Run:  XMRGATE_URL=https://pay.example.com XMRGATE_KEY=... SHOP_URL=http://localhost:8000 \
//       php -S localhost:8000 shop.php

$gateway = rtrim(getenv('XMRGATE_URL'), '/');
$key = getenv('XMRGATE_KEY');
$shop = rtrim(getenv('SHOP_URL'), '/');
$path = parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH);

if ($path === '/buy') {
    $order = 'order-' . time();
    $request = curl_init("$gateway/api/v1/invoices");
    curl_setopt_array($request, [
        CURLOPT_POST => true,
        CURLOPT_RETURNTRANSFER => true,
        CURLOPT_HTTPHEADER => ["Authorization: Bearer $key", 'Content-Type: application/json'],
        CURLOPT_POSTFIELDS => json_encode([
            'amount' => '19.99',
            'currency' => 'EUR',
            'order_ref' => $order,
            'callback_url' => "$shop/webhook",
            'redirect_url' => "$shop/thanks",
        ]),
    ]);
    $invoice = json_decode(curl_exec($request), true);
    if (empty($invoice['checkout_url'])) {
        http_response_code(502);
        exit('Monero payments are unavailable right now.');
    }
    header('Location: ' . $invoice['checkout_url'], true, 303);
    exit;
}

if ($path === '/webhook') {
    $body = file_get_contents('php://input');
    $signature = $_SERVER['HTTP_X_XMRGATE_SIGNATURE'] ?? '';
    // Only xmrgate knows the key, so a valid signature proves the update is genuine.
    if (!hash_equals(hash_hmac('sha256', $body, $key), $signature)) {
        http_response_code(401);
        exit;
    }
    $invoice = json_decode($body, true)['invoice'];
    if ($invoice['status'] === 'confirmed') {
        // Mark the order as paid here.
        error_log("PAID {$invoice['order_ref']} {$invoice['received_xmr']} XMR");
    } else {
        error_log("UPDATE {$invoice['order_ref']} {$invoice['status']}");
    }
    http_response_code(200);
    exit;
}

echo $path === '/thanks' ? 'Thank you!' : '<a href="/buy">Buy for 19.99 EUR</a>';
