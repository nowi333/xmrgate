<?php
/**
 * The Monero payment method.
 *
 * The customer is sent to the payment page of the shop owner's xmrgate gateway. The gateway
 * then reports each change of the payment through a signed webhook.
 */

defined( 'ABSPATH' ) || exit;

class WC_Gateway_Xmrgate extends WC_Payment_Gateway {

	const INVOICE_META = '_xmrgate_invoice_id';

	public function __construct() {
		$this->id                 = 'xmrgate';
		$this->method_title       = __( 'Monero (xmrgate)', 'xmrgate' );
		$this->method_description = __( 'Accept Monero through your own xmrgate gateway. Payments go straight to your wallet, with no intermediary and no fee.', 'xmrgate' );
		$this->has_fields         = false;
		$this->supports           = array( 'products' );

		$this->init_form_fields();
		$this->init_settings();

		$this->title       = $this->get_option( 'title' );
		$this->description = trim( $this->get_option( 'description' ) . ' ' . $this->adjustment_notice() );

		add_action( 'woocommerce_update_options_payment_gateways_' . $this->id, array( $this, 'process_admin_options' ) );
	}

	public function init_form_fields() {
		$this->form_fields = array(
			'enabled'     => array(
				'title'   => __( 'Enable', 'xmrgate' ),
				'type'    => 'checkbox',
				'label'   => __( 'Accept Monero payments', 'xmrgate' ),
				'default' => 'yes',
			),
			'gateway_url' => array(
				'title'       => __( 'Gateway address', 'xmrgate' ),
				'type'        => 'text',
				'description' => __( 'Copy it from your xmrgate dashboard, under "Connect a website".', 'xmrgate' ),
				'placeholder' => 'https://pay.example.com',
				'default'     => '',
			),
			'api_key'     => array(
				'title'       => __( 'Secret key', 'xmrgate' ),
				'type'        => 'password',
				'description' => __( 'Copy it from your xmrgate dashboard, under "Connect a website".', 'xmrgate' ),
				'default'     => '',
			),
			'title'       => array(
				'title'   => __( 'Name shown at checkout', 'xmrgate' ),
				'type'    => 'text',
				'default' => __( 'Monero (XMR)', 'xmrgate' ),
			),
			'description' => array(
				'title'   => __( 'Text shown at checkout', 'xmrgate' ),
				'type'    => 'textarea',
				'default' => __( 'Pay privately with Monero. You will be shown a QR code to scan with your wallet.', 'xmrgate' ),
			),
			'adjustment'  => array(
				'title'             => __( 'Discount or fee (%)', 'xmrgate' ),
				'type'              => 'number',
				'description'       => __( 'Change the price for customers who pay with Monero. A negative number is a discount (-5 gives 5% off), a positive number is a fee (2 adds 2%). Leave 0 for none. It applies to the products and shipping, before tax.', 'xmrgate' ),
				'default'           => '0',
				'custom_attributes' => array(
					'step' => '0.01',
					'min'  => '-100',
					'max'  => '100',
				),
			),
		);
	}

	/**
	 * Save the settings, then check that the gateway answers and accepts the key, so a typo is
	 * caught now and not by the first customer.
	 */
	public function process_admin_options() {
		$saved = parent::process_admin_options();

		$url = $this->gateway_url();
		$key = $this->get_option( 'api_key' );
		if ( '' === $url || '' === $key ) {
			WC_Admin_Settings::add_error( __( 'Monero: enter the gateway address and the secret key from your xmrgate dashboard.', 'xmrgate' ) );
			return $saved;
		}

		// Asking for an invoice that cannot exist tells the three cases apart.
		$response = $this->request( 'GET', '/api/v1/invoices/connection-check' );
		if ( is_wp_error( $response ) ) {
			WC_Admin_Settings::add_error(
				sprintf(
					/* translators: %s: technical error message */
					__( 'Monero: the gateway could not be reached. Check the gateway address. (%s)', 'xmrgate' ),
					$response->get_error_message()
				)
			);
		} elseif ( 401 === $response['code'] ) {
			WC_Admin_Settings::add_error( __( 'Monero: the gateway refused the secret key. Copy it again from your xmrgate dashboard.', 'xmrgate' ) );
		} elseif ( 404 === $response['code'] && isset( $response['body']['error'] ) ) {
			WC_Admin_Settings::add_message( __( 'Monero: connected to your xmrgate gateway.', 'xmrgate' ) );
		} else {
			WC_Admin_Settings::add_error( __( 'Monero: this address did not answer like an xmrgate gateway. Check the gateway address.', 'xmrgate' ) );
		}
		return $saved;
	}

	/**
	 * The discount (negative) or fee (positive) in percent, between -100 and 100.
	 */
	public function adjustment_percent() {
		$value = (float) str_replace( ',', '.', (string) $this->get_option( 'adjustment', '0' ) );
		return max( -100.0, min( 100.0, $value ) );
	}

	/**
	 * The name of the discount or fee line, shown in the cart, the order and the emails.
	 */
	public function adjustment_label() {
		$percent = wc_format_localized_decimal( (string) abs( $this->adjustment_percent() ) );
		return $this->adjustment_percent() < 0
			/* translators: %s: percentage */
			? sprintf( __( 'Monero discount (%s%%)', 'xmrgate' ), $percent )
			/* translators: %s: percentage */
			: sprintf( __( 'Monero fee (%s%%)', 'xmrgate' ), $percent );
	}

	/**
	 * The sentence added to the checkout text when a discount or fee is set.
	 */
	private function adjustment_notice() {
		$adjustment = $this->adjustment_percent();
		if ( 0.0 === $adjustment ) {
			return '';
		}
		$percent = wc_format_localized_decimal( (string) abs( $adjustment ) );
		return $adjustment < 0
			/* translators: %s: percentage */
			? sprintf( __( 'You get %s%% off when you pay with Monero.', 'xmrgate' ), $percent )
			/* translators: %s: percentage */
			: sprintf( __( 'A %s%% fee applies when you pay with Monero.', 'xmrgate' ), $percent );
	}

	/**
	 * The discount or fee amount for a base of products plus shipping, before tax.
	 */
	public function adjustment_amount( $base ) {
		return round( (float) $base * $this->adjustment_percent() / 100, wc_get_price_decimals() );
	}

	/**
	 * Put the discount or fee on the order, replacing any earlier one. Done when the customer
	 * pays, so the amount is right whatever checkout the shop uses and on every retry.
	 */
	private function apply_adjustment( $order ) {
		$changed = false;
		foreach ( $order->get_fees() as $item_id => $fee ) {
			// The classic checkout already copied the cart line, under the same name.
			if ( $fee->get_meta( '_xmrgate_adjustment' ) || $fee->get_name() === $this->adjustment_label() ) {
				$order->remove_item( $item_id );
				$changed = true;
			}
		}

		$base   = (float) $order->get_subtotal() - (float) $order->get_discount_total() + (float) $order->get_shipping_total();
		$amount = $this->adjustment_amount( $base );
		if ( 0.0 !== $amount ) {
			$fee = new WC_Order_Item_Fee();
			$fee->set_name( $this->adjustment_label() );
			$fee->set_amount( $amount );
			$fee->set_total( $amount );
			$fee->set_tax_status( 'none' );
			$fee->add_meta_data( '_xmrgate_adjustment', '1', true );
			$order->add_item( $fee );
			$changed = true;
		}

		if ( $changed ) {
			$order->calculate_totals( false );
			$order->save();
		}
	}

	public function is_available() {
		return parent::is_available() && '' !== $this->gateway_url() && '' !== $this->get_option( 'api_key' );
	}

	private function gateway_url() {
		return untrailingslashit( trim( (string) $this->get_option( 'gateway_url' ) ) );
	}

	/**
	 * Call the gateway API.
	 *
	 * @return array|WP_Error `code` and decoded `body` on any HTTP answer.
	 */
	private function request( $method, $path, $body = null ) {
		$args = array(
			'method'  => $method,
			'timeout' => 20,
			'headers' => array(
				'Authorization' => 'Bearer ' . $this->get_option( 'api_key' ),
				'Content-Type'  => 'application/json',
			),
		);
		if ( null !== $body ) {
			$args['body'] = wp_json_encode( $body );
		}
		$response = wp_remote_request( $this->gateway_url() . $path, $args );
		if ( is_wp_error( $response ) ) {
			return $response;
		}
		$decoded = json_decode( wp_remote_retrieve_body( $response ), true );
		return array(
			'code' => (int) wp_remote_retrieve_response_code( $response ),
			'body' => is_array( $decoded ) ? $decoded : array(),
		);
	}

	public function process_payment( $order_id ) {
		$order = wc_get_order( $order_id );
		$this->apply_adjustment( $order );

		$response = $this->request(
			'POST',
			'/api/v1/invoices',
			array(
				'amount'       => number_format( (float) $order->get_total(), wc_get_price_decimals(), '.', '' ),
				'currency'     => $order->get_currency(),
				'order_ref'    => (string) $order->get_order_number(),
				'description'  => get_bloginfo( 'name' ),
				'callback_url' => WC()->api_request_url( 'xmrgate' ),
				'redirect_url' => $this->get_return_url( $order ),
			)
		);

		if ( is_wp_error( $response ) || 201 !== $response['code'] || empty( $response['body']['checkout_url'] ) ) {
			$reason = is_wp_error( $response ) ? $response->get_error_message() : wp_json_encode( $response['body'] );
			wc_get_logger()->error( 'Could not create the Monero payment: ' . $reason, array( 'source' => 'xmrgate' ) );
			$rate = ! is_wp_error( $response ) && isset( $response['body']['error'] ) && 'rate_unavailable' === $response['body']['error'];
			wc_add_notice(
				$rate
					? __( 'Monero payments are not available in this currency right now. Please choose another payment method.', 'xmrgate' )
					: __( 'Monero payments are temporarily unavailable. Please try again in a moment or choose another payment method.', 'xmrgate' ),
				'error'
			);
			return array( 'result' => 'failure' );
		}

		$invoice = $response['body'];
		// A customer can retry: every invoice of the order is kept, so a payment to an earlier
		// one is still credited.
		$order->add_meta_data( self::INVOICE_META, $invoice['id'] );
		$order->add_order_note(
			sprintf(
				/* translators: 1: amount in XMR, 2: invoice id */
				__( 'Monero payment requested: %1$s XMR (invoice %2$s).', 'xmrgate' ),
				$invoice['amount_xmr'],
				$invoice['id']
			)
		);
		$order->save();

		return array(
			'result'   => 'success',
			'redirect' => $invoice['checkout_url'],
		);
	}

	/**
	 * Receive a payment update from the gateway.
	 *
	 * The body is signed with the secret key. The invoice is then read back from the gateway, so
	 * the order only ever changes on the gateway's own current answer.
	 */
	public function handle_webhook() {
		$body      = file_get_contents( 'php://input' );
		$signature = isset( $_SERVER['HTTP_X_XMRGATE_SIGNATURE'] ) ? sanitize_text_field( wp_unslash( $_SERVER['HTTP_X_XMRGATE_SIGNATURE'] ) ) : '';
		$expected  = hash_hmac( 'sha256', (string) $body, (string) $this->get_option( 'api_key' ) );
		if ( '' === $signature || ! hash_equals( $expected, $signature ) ) {
			status_header( 401 );
			exit;
		}

		$payload    = json_decode( (string) $body, true );
		$invoice_id = isset( $payload['invoice']['id'] ) ? sanitize_text_field( $payload['invoice']['id'] ) : '';
		if ( '' === $invoice_id ) {
			status_header( 400 );
			exit;
		}

		$orders = wc_get_orders(
			array(
				'limit'      => 1,
				'meta_key'   => self::INVOICE_META, // phpcs:ignore WordPress.DB.SlowDBQuery
				'meta_value' => $invoice_id, // phpcs:ignore WordPress.DB.SlowDBQuery
			)
		);
		if ( empty( $orders ) ) {
			// Not one of ours: acknowledge so the gateway stops retrying.
			status_header( 200 );
			exit;
		}
		$order = $orders[0];

		$response = $this->request( 'GET', '/api/v1/invoices/' . rawurlencode( $invoice_id ) );
		if ( is_wp_error( $response ) || 200 !== $response['code'] || empty( $response['body']['status'] ) ) {
			// Ask the gateway to try again later.
			status_header( 503 );
			exit;
		}

		$this->apply_status( $order, $response['body'] );
		status_header( 200 );
		exit;
	}

	private function apply_status( $order, $invoice ) {
		$id = $invoice['id'];
		switch ( $invoice['status'] ) {
			case 'confirmed':
				if ( ! $order->is_paid() ) {
					$order->add_order_note(
						/* translators: %s: amount in XMR */
						sprintf( __( 'Monero payment confirmed: %s XMR received.', 'xmrgate' ), $invoice['received_xmr'] )
					);
					$order->payment_complete( $id );
				}
				break;

			case 'seen':
				if ( $order->is_paid() ) {
					// A confirmed payment lost its confirmations: a rare chain reorganisation.
					$order->update_status( 'on-hold', __( 'The Monero payment is being confirmed again after a network reorganisation. Wait before shipping.', 'xmrgate' ) );
				} elseif ( $order->has_status( array( 'pending', 'failed', 'cancelled' ) ) ) {
					$order->update_status( 'on-hold', __( 'Monero payment detected, waiting for confirmation.', 'xmrgate' ) );
				}
				break;

			case 'underpaid':
				if ( ! $order->is_paid() ) {
					$order->update_status(
						'on-hold',
						sprintf(
							/* translators: 1: received XMR, 2: expected XMR, 3: missing XMR */
							__( 'Partial Monero payment: %1$s of %2$s XMR received, %3$s XMR missing.', 'xmrgate' ),
							$invoice['received_xmr'],
							$invoice['amount_xmr'],
							$invoice['due_xmr']
						)
					);
				}
				break;

			case 'expired':
				// Only cancel when this is the order's latest payment attempt.
				$ids = $order->get_meta( self::INVOICE_META, false );
				$last = end( $ids );
				if ( $order->has_status( 'pending' ) && $last && $last->value === $id ) {
					$order->update_status( 'cancelled', __( 'The Monero payment request expired without payment.', 'xmrgate' ) );
				}
				break;

			case 'pending':
				// A detected payment disappeared before being confirmed.
				if ( $order->has_status( 'on-hold' ) ) {
					$order->update_status( 'pending', __( 'The Monero payment that was detected never confirmed. Nothing was received.', 'xmrgate' ) );
				}
				break;
		}
	}
}
