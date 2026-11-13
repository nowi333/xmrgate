<?php
/**
 * The customer chose Monero: ask the gateway for a payment, create the order, and send the
 * customer to the payment page.
 *
 * @license MIT
 */

if (!defined('_PS_VERSION_')) {
    exit;
}

class XmrgateValidationModuleFrontController extends ModuleFrontController
{
    public function postProcess()
    {
        /** @var Xmrgate $module */
        $module = $this->module;
        $cart = $this->context->cart;

        if (!$cart->id || $cart->id_customer == 0 || $cart->id_address_delivery == 0 || $cart->id_address_invoice == 0 || !$module->isConfigured()) {
            Tools::redirect($this->context->link->getPageLink('order', true));
        }
        $authorized = false;
        foreach (Module::getPaymentModules() as $payment) {
            if ($payment['name'] === $module->name) {
                $authorized = true;
                break;
            }
        }
        $customer = new Customer((int) $cart->id_customer);
        if (!$authorized || !Validate::isLoadedObject($customer)) {
            Tools::redirect($this->context->link->getPageLink('order', true));
        }

        $currency = new Currency((int) $cart->id_currency);
        $total = (float) $cart->getOrderTotal(true, Cart::BOTH);
        $decimals = isset($currency->precision) ? (int) $currency->precision : 2;

        // The invoice comes first: if the gateway is down, no order is created and the
        // customer can simply choose another payment method.
        $response = $module->request('POST', '/api/v1/invoices', [
            'amount' => number_format($total, $decimals, '.', ''),
            'currency' => $currency->iso_code,
            'order_ref' => 'cart ' . (int) $cart->id,
            'description' => (string) Configuration::get('PS_SHOP_NAME'),
            'callback_url' => $this->context->link->getModuleLink($module->name, 'webhook', [], true),
            'redirect_url' => $this->context->link->getModuleLink($module->name, 'return', ['id_cart' => (int) $cart->id, 'key' => $customer->secure_key], true),
        ]);

        if (isset($response['error']) || $response['code'] !== 201 || empty($response['body']['checkout_url'])) {
            $reason = isset($response['error']) ? $response['error'] : json_encode($response['body']);
            PrestaShopLogger::addLog('xmrgate: could not create the Monero payment: ' . $reason, 3, null, 'Cart', (int) $cart->id, true);
            $rate = isset($response['body']['error']) && $response['body']['error'] === 'rate_unavailable';
            $this->errors[] = $rate
                ? $module->l('Monero payments are not available in this currency right now. Please choose another payment method.', 'validation')
                : $module->l('Monero payments are temporarily unavailable. Please try again in a moment or choose another payment method.', 'validation');
            $this->redirectWithNotifications($this->context->link->getPageLink('order', true));
        }

        $invoice = $response['body'];
        $module->validateOrder(
            (int) $cart->id,
            (int) Configuration::get(Xmrgate::STATE_AWAITING),
            $total,
            $module->displayName,
            sprintf('Monero payment requested: %s XMR (invoice %s).', $invoice['amount_xmr'], $invoice['id']),
            ['transaction_id' => $invoice['id']],
            (int) $currency->id,
            false,
            $customer->secure_key
        );
        $module->rememberInvoice($invoice['id'], (int) $module->currentOrder, (int) $cart->id);

        Tools::redirect($invoice['checkout_url']);
    }
}
