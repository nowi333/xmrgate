<?php
/**
 * Plugin Name: xmrgate - Monero payments for WooCommerce
 * Description: Accept Monero (XMR) in your shop through your own xmrgate gateway. Payments go straight to your wallet.
 * Version: 0.1.0
 * Requires at least: 6.2
 * Requires PHP: 7.4
 * Requires Plugins: woocommerce
 * License: MIT
 * Text Domain: xmrgate
 */

defined( 'ABSPATH' ) || exit;

define( 'XMRGATE_PLUGIN_FILE', __FILE__ );
define( 'XMRGATE_VERSION', '0.1.0' );

add_action(
	'before_woocommerce_init',
	function () {
		if ( class_exists( '\Automattic\WooCommerce\Utilities\FeaturesUtil' ) ) {
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'custom_order_tables', __FILE__, true );
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'cart_checkout_blocks', __FILE__, true );
		}
	}
);

add_action(
	'plugins_loaded',
	function () {
		if ( ! class_exists( 'WC_Payment_Gateway' ) ) {
			return;
		}
		require_once __DIR__ . '/includes/class-wc-gateway-xmrgate.php';

		add_filter(
			'woocommerce_payment_gateways',
			function ( $gateways ) {
				$gateways[] = 'WC_Gateway_Xmrgate';
				return $gateways;
			}
		);

		// The webhook must be answered even when the gateway object was not otherwise loaded.
		add_action(
			'woocommerce_api_xmrgate',
			function () {
				$gateways = WC()->payment_gateways()->payment_gateways();
				if ( isset( $gateways['xmrgate'] ) ) {
					$gateways['xmrgate']->handle_webhook();
				}
				status_header( 503 );
				exit;
			}
		);
	}
);

/**
 * The Monero gateway object, when WooCommerce has it enabled.
 */
function xmrgate_gateway() {
	if ( ! function_exists( 'WC' ) || ! WC()->payment_gateways() ) {
		return null;
	}
	$gateways = WC()->payment_gateways()->payment_gateways();
	return isset( $gateways['xmrgate'] ) && 'yes' === $gateways['xmrgate']->enabled ? $gateways['xmrgate'] : null;
}

// Show the Monero discount or fee in the classic cart and checkout totals once Monero is chosen.
// The order itself gets it when the customer pays, whatever checkout the shop uses.
add_action(
	'woocommerce_cart_calculate_fees',
	function ( $cart ) {
		$gateway = xmrgate_gateway();
		if ( ! $gateway || 0.0 === $gateway->adjustment_percent() || ! WC()->session ) {
			return;
		}
		if ( 'xmrgate' !== WC()->session->get( 'chosen_payment_method' ) ) {
			return;
		}
		$base   = (float) $cart->get_cart_contents_total() + (float) $cart->get_shipping_total();
		$amount = $gateway->adjustment_amount( $base );
		if ( 0.0 !== $amount ) {
			$cart->add_fee( $gateway->adjustment_label(), $amount, false );
		}
	}
);

// The classic checkout does not refresh its totals when the payment method changes.
add_action(
	'wp_enqueue_scripts',
	function () {
		$gateway = xmrgate_gateway();
		if ( ! $gateway || 0.0 === $gateway->adjustment_percent() || ! function_exists( 'is_checkout' ) || ! is_checkout() ) {
			return;
		}
		wp_add_inline_script(
			'wc-checkout',
			"jQuery( function ( $ ) { $( 'form.checkout' ).on( 'change', 'input[name=\"payment_method\"]', function () { $( document.body ).trigger( 'update_checkout' ); } ); } );"
		);
	}
);

add_action(
	'woocommerce_blocks_loaded',
	function () {
		if ( ! class_exists( '\Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType' ) ) {
			return;
		}
		require_once __DIR__ . '/includes/class-xmrgate-blocks.php';
		add_action(
			'woocommerce_blocks_payment_method_type_registration',
			function ( $registry ) {
				$registry->register( new Xmrgate_Blocks_Support() );
			}
		);
	}
);

add_filter(
	'plugin_action_links_' . plugin_basename( __FILE__ ),
	function ( $links ) {
		$url = admin_url( 'admin.php?page=wc-settings&tab=checkout&section=xmrgate' );
		array_unshift( $links, '<a href="' . esc_url( $url ) . '">' . esc_html__( 'Settings', 'xmrgate' ) . '</a>' );
		return $links;
	}
);
