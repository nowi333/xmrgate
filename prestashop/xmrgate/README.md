# xmrgate for PrestaShop

Accept Monero (XMR) in your PrestaShop store through your own xmrgate gateway. Payments go
straight to your own wallet.

## Install

1. Start xmrgate and finish its one-screen setup.
2. In the xmrgate dashboard, under **Connect a website**, add a website for your store and
   click **Download the PrestaShop module**.
3. In PrestaShop, go to **Modules → Module Manager → Upload a module** and choose the zip.
4. Click **Configure**, paste the gateway address and the website's secret key, then save.
   The page tells you at once whether the connection works.

Requires PrestaShop 1.7.7 or later (8.x and 9.x included) and PHP with cURL.

## What happens to orders

| What happened | Order status |
|---|---|
| The customer chose Monero | **Awaiting Monero payment** |
| The payment was detected | **Monero payment detected, confirming** |
| The payment is confirmed | **Payment accepted** |
| The customer paid only part of it | **Monero payment incomplete** |
| Nobody paid in time | **Canceled** |
