=== xmrgate - Monero payments for WooCommerce ===
Requires at least: 6.2
Tested up to: 6.8
Requires PHP: 7.4
Stable tag: 0.1.0
License: MIT

Accept Monero (XMR) in your WooCommerce shop through your own xmrgate gateway.

== Description ==

Payments go straight to your own wallet. There is no intermediary, no fee and no Monero node
to run. This plugin connects your shop to an xmrgate gateway that you host.

== Installation ==

1. Start xmrgate and finish its one-screen setup.
2. In WordPress, go to Plugins, Add new, Upload, and choose this plugin's zip file. Activate it.
3. Go to WooCommerce, Settings, Payments, Monero (xmrgate).
4. Paste the gateway address and the secret key shown in your xmrgate dashboard, then save.

The settings page tells you at once whether the connection works.

== Discount or fee ==

"Discount or fee (%)" changes the price for customers who pay with Monero: -5 gives 5% off,
2 adds a 2% fee, 0 means neither. It applies to the products and shipping, before tax, and
shows as its own line in the checkout, the order and the emails.
