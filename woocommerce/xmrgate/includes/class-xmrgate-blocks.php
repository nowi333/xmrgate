<?php
/**
 * Makes the Monero payment method appear in the block-based checkout.
 */

defined( 'ABSPATH' ) || exit;

use Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType;

final class Xmrgate_Blocks_Support extends AbstractPaymentMethodType {

	protected $name = 'xmrgate';

	public function initialize() {
		$this->settings = get_option( 'woocommerce_xmrgate_settings', array() );
	}

	public function is_active() {
		$gateways = WC()->payment_gateways()->payment_gateways();
		return isset( $gateways['xmrgate'] ) && $gateways['xmrgate']->is_available();
	}

	public function get_payment_method_script_handles() {
		wp_register_script(
			'xmrgate-blocks',
			plugins_url( 'assets/blocks.js', XMRGATE_PLUGIN_FILE ),
			array( 'wc-blocks-registry', 'wc-settings', 'wp-element', 'wp-html-entities' ),
			XMRGATE_VERSION,
			true
		);
		return array( 'xmrgate-blocks' );
	}

	public function get_payment_method_data() {
		return array(
			'title'       => $this->get_setting( 'title', 'Monero (XMR)' ),
			'description' => $this->get_setting( 'description', '' ),
			'supports'    => array( 'products' ),
		);
	}
}
