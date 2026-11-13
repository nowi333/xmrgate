<?php
/**
 * xmrgate - Monero payments for PrestaShop.
 *
 * The customer is sent to the payment page of the shop owner's xmrgate gateway. The gateway
 * then reports each change of the payment through a signed webhook, and the order follows.
 *
 * @license MIT
 */

use PrestaShop\PrestaShop\Core\Payment\PaymentOption;

if (!defined('_PS_VERSION_')) {
    exit;
}

class Xmrgate extends PaymentModule
{
    const CONFIG_URL = 'XMRGATE_GATEWAY_URL';
    const CONFIG_KEY = 'XMRGATE_API_KEY';
    const CONFIG_TITLE = 'XMRGATE_TITLE';
    const STATE_AWAITING = 'XMRGATE_OS_AWAITING';
    const STATE_CONFIRMING = 'XMRGATE_OS_CONFIRMING';
    const STATE_UNDERPAID = 'XMRGATE_OS_UNDERPAID';

    public function __construct()
    {
        $this->name = 'xmrgate';
        $this->tab = 'payments_gateways';
        $this->version = '0.1.0';
        $this->author = 'xmrgate';
        $this->need_instance = 0;
        $this->ps_versions_compliancy = ['min' => '1.7.7.0', 'max' => _PS_VERSION_];
        $this->bootstrap = true;
        $this->controllers = ['validation', 'webhook', 'return'];
        $this->currencies = true;
        $this->currencies_mode = 'checkbox';

        parent::__construct();

        $this->displayName = $this->l('Monero (xmrgate)');
        $this->description = $this->l('Accept Monero through your own xmrgate gateway. Payments go straight to your wallet, with no intermediary and no fee.');
        $this->confirmUninstall = $this->l('Monero payments will stop. Past orders are kept. Continue?');
    }

    /* ------------------------------------------------------------------ */
    /* Install                                                            */
    /* ------------------------------------------------------------------ */

    public function install()
    {
        return parent::install()
            && $this->registerHook('paymentOptions')
            && $this->registerHook('displayPaymentReturn')
            && $this->installStates()
            && $this->installTable()
            && Configuration::updateValue(self::CONFIG_TITLE, 'Monero (XMR)');
    }

    public function uninstall()
    {
        // The order states stay: past orders still point to them. They are only hidden.
        foreach ([self::STATE_AWAITING, self::STATE_CONFIRMING, self::STATE_UNDERPAID] as $key) {
            $state = new OrderState((int) Configuration::get($key));
            if (Validate::isLoadedObject($state)) {
                $state->deleted = true;
                $state->save();
            }
            Configuration::deleteByName($key);
        }
        Configuration::deleteByName(self::CONFIG_URL);
        Configuration::deleteByName(self::CONFIG_KEY);
        Configuration::deleteByName(self::CONFIG_TITLE);

        return parent::uninstall();
    }

    private function installTable()
    {
        return Db::getInstance()->execute(
            'CREATE TABLE IF NOT EXISTS `' . _DB_PREFIX_ . 'xmrgate_invoice` (
                `invoice_id` VARCHAR(64) NOT NULL,
                `id_order` INT UNSIGNED NOT NULL,
                `id_cart` INT UNSIGNED NOT NULL,
                `date_add` DATETIME NOT NULL,
                PRIMARY KEY (`invoice_id`),
                KEY `id_order` (`id_order`)
            ) ENGINE=' . _MYSQL_ENGINE_ . ' DEFAULT CHARSET=utf8mb4'
        );
    }

    private function installStates()
    {
        $states = [
            self::STATE_AWAITING => ['Awaiting Monero payment', '#f26822', false],
            self::STATE_CONFIRMING => ['Monero payment detected, confirming', '#4169e1', false],
            self::STATE_UNDERPAID => ['Monero payment incomplete', '#8f0621', false],
        ];
        foreach ($states as $key => list($name, $color, $paid)) {
            $existing = new OrderState((int) Configuration::get($key));
            if (Validate::isLoadedObject($existing)) {
                $existing->deleted = false;
                $existing->save();
                continue;
            }
            $state = new OrderState();
            $state->name = [];
            foreach (Language::getLanguages(false) as $language) {
                $state->name[(int) $language['id_lang']] = $name;
            }
            $state->module_name = $this->name;
            $state->color = $color;
            $state->send_email = false;
            $state->hidden = false;
            $state->delivery = false;
            $state->logable = false;
            $state->invoice = false;
            $state->paid = $paid;
            $state->unremovable = true;
            if (!$state->add()) {
                return false;
            }
            Configuration::updateValue($key, (int) $state->id);
        }

        return true;
    }

    /* ------------------------------------------------------------------ */
    /* Settings page                                                      */
    /* ------------------------------------------------------------------ */

    public function getContent()
    {
        $output = '';
        if (Tools::isSubmit('submitXmrgate')) {
            $url = trim((string) Tools::getValue(self::CONFIG_URL));
            $key = trim((string) Tools::getValue(self::CONFIG_KEY));
            $title = trim((string) Tools::getValue(self::CONFIG_TITLE));
            Configuration::updateValue(self::CONFIG_URL, rtrim($url, '/'));
            // An empty key field means "keep the saved one": it is never shown back.
            if ($key !== '') {
                Configuration::updateValue(self::CONFIG_KEY, $key);
            }
            Configuration::updateValue(self::CONFIG_TITLE, $title !== '' ? $title : 'Monero (XMR)');
            $output .= $this->connectionMessage();
        }

        return $output . $this->renderForm();
    }

    /**
     * Check that the gateway answers and accepts the key, so a typo is caught now and not by
     * the first customer.
     */
    private function connectionMessage()
    {
        if ($this->gatewayUrl() === '' || (string) Configuration::get(self::CONFIG_KEY) === '') {
            return $this->displayError($this->l('Enter the gateway address and the secret key from your xmrgate dashboard.'));
        }
        // Asking for an invoice that cannot exist tells the three cases apart.
        $response = $this->request('GET', '/api/v1/invoices/connection-check');
        if (isset($response['error'])) {
            return $this->displayError(sprintf($this->l('The gateway could not be reached. Check the gateway address. (%s)'), $response['error']));
        }
        if ($response['code'] === 401) {
            return $this->displayError($this->l('The gateway refused the secret key. Copy it again from your xmrgate dashboard.'));
        }
        if ($response['code'] === 404 && isset($response['body']['error'])) {
            return $this->displayConfirmation($this->l('Connected to your xmrgate gateway.'));
        }

        return $this->displayError($this->l('This address did not answer like an xmrgate gateway. Check the gateway address.'));
    }

    private function renderForm()
    {
        $form = [
            'form' => [
                'legend' => ['title' => $this->l('Monero (xmrgate)'), 'icon' => 'icon-cogs'],
                'description' => $this->l('Copy the gateway address and the secret key from your xmrgate dashboard, under "Connect a website".'),
                'input' => [
                    [
                        'type' => 'text',
                        'label' => $this->l('Gateway address'),
                        'name' => self::CONFIG_URL,
                        'placeholder' => 'https://pay.example.com',
                        'required' => true,
                    ],
                    [
                        'type' => 'password',
                        'label' => $this->l('Secret key'),
                        'name' => self::CONFIG_KEY,
                        'desc' => Configuration::get(self::CONFIG_KEY)
                            ? $this->l('A key is saved. Leave empty to keep it.')
                            : $this->l('Copy it from your xmrgate dashboard.'),
                    ],
                    [
                        'type' => 'text',
                        'label' => $this->l('Name shown at checkout'),
                        'name' => self::CONFIG_TITLE,
                    ],
                ],
                'submit' => ['title' => $this->l('Save')],
            ],
        ];

        $helper = new HelperForm();
        $helper->module = $this;
        $helper->name_controller = $this->name;
        $helper->token = Tools::getAdminTokenLite('AdminModules');
        $helper->currentIndex = AdminController::$currentIndex . '&configure=' . $this->name;
        $helper->submit_action = 'submitXmrgate';
        $helper->default_form_language = (int) Configuration::get('PS_LANG_DEFAULT');
        $helper->fields_value = [
            self::CONFIG_URL => Configuration::get(self::CONFIG_URL),
            self::CONFIG_KEY => '',
            self::CONFIG_TITLE => Configuration::get(self::CONFIG_TITLE),
        ];

        return $helper->generateForm([$form]);
    }

    /* ------------------------------------------------------------------ */
    /* Checkout                                                           */
    /* ------------------------------------------------------------------ */

    public function isConfigured()
    {
        return $this->active && $this->gatewayUrl() !== '' && (string) Configuration::get(self::CONFIG_KEY) !== '';
    }

    public function hookPaymentOptions($params)
    {
        if (!$this->isConfigured() || !$this->checkCurrency($params['cart'])) {
            return [];
        }
        $option = new PaymentOption();
        $option->setModuleName($this->name)
            ->setCallToActionText((string) Configuration::get(self::CONFIG_TITLE) ?: 'Monero (XMR)')
            ->setAction($this->context->link->getModuleLink($this->name, 'validation', [], true))
            ->setAdditionalInformation('<p>' . $this->l('Pay privately with Monero. You will be shown a QR code to scan with your wallet.') . '</p>');

        return [$option];
    }

    private function checkCurrency($cart)
    {
        $currency = new Currency((int) $cart->id_currency);
        foreach ((array) $this->getCurrency((int) $cart->id_currency) as $allowed) {
            if ((int) $currency->id === (int) $allowed['id_currency']) {
                return true;
            }
        }

        return false;
    }

    public function hookDisplayPaymentReturn($params)
    {
        if (!$this->active || empty($params['order']) || $params['order']->module !== $this->name) {
            return '';
        }
        $order = $params['order'];
        $state = (int) $order->getCurrentState();
        if ($order->hasBeenPaid()) {
            $text = $this->l('Your Monero payment is confirmed. Thank you!');
        } elseif ($state === (int) Configuration::get(self::STATE_CONFIRMING)) {
            $text = $this->l('Your Monero payment was received and is being confirmed by the network. You will get an email once it is confirmed.');
        } else {
            $text = $this->l('Your order is waiting for its Monero payment.');
        }

        return '<p class="alert alert-info">' . htmlspecialchars($text, ENT_QUOTES, 'UTF-8') . '</p>';
    }

    /* ------------------------------------------------------------------ */
    /* Gateway                                                            */
    /* ------------------------------------------------------------------ */

    public function gatewayUrl()
    {
        return rtrim(trim((string) Configuration::get(self::CONFIG_URL)), '/');
    }

    public function apiKey()
    {
        return (string) Configuration::get(self::CONFIG_KEY);
    }

    /**
     * Call the gateway API.
     *
     * @return array `code` and decoded `body` on any HTTP answer, or `error` when unreachable
     */
    public function request($method, $path, $body = null)
    {
        $curl = curl_init($this->gatewayUrl() . $path);
        $headers = ['Authorization: Bearer ' . $this->apiKey(), 'Accept: application/json'];
        $options = [
            CURLOPT_CUSTOMREQUEST => $method,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT => 20,
            CURLOPT_CONNECTTIMEOUT => 10,
        ];
        if ($body !== null) {
            $headers[] = 'Content-Type: application/json';
            $options[CURLOPT_POSTFIELDS] = json_encode($body);
        }
        $options[CURLOPT_HTTPHEADER] = $headers;
        curl_setopt_array($curl, $options);
        $raw = curl_exec($curl);
        if ($raw === false) {
            $error = curl_error($curl);
            curl_close($curl);

            return ['error' => $error];
        }
        $code = (int) curl_getinfo($curl, CURLINFO_HTTP_CODE);
        curl_close($curl);
        $decoded = json_decode((string) $raw, true);

        return ['code' => $code, 'body' => is_array($decoded) ? $decoded : []];
    }

    public function rememberInvoice($invoiceId, $idOrder, $idCart)
    {
        return Db::getInstance()->insert('xmrgate_invoice', [
            'invoice_id' => pSQL($invoiceId),
            'id_order' => (int) $idOrder,
            'id_cart' => (int) $idCart,
            'date_add' => date('Y-m-d H:i:s'),
        ]);
    }

    public function orderIdForInvoice($invoiceId)
    {
        return (int) Db::getInstance()->getValue(
            'SELECT `id_order` FROM `' . _DB_PREFIX_ . "xmrgate_invoice` WHERE `invoice_id` = '" . pSQL($invoiceId) . "'"
        );
    }

    /**
     * Move the order to match the invoice as the gateway reports it.
     */
    public function applyStatus(Order $order, array $invoice)
    {
        $awaiting = (int) Configuration::get(self::STATE_AWAITING);
        $confirming = (int) Configuration::get(self::STATE_CONFIRMING);
        $underpaid = (int) Configuration::get(self::STATE_UNDERPAID);
        $current = (int) $order->getCurrentState();
        $paid = $order->hasBeenPaid();

        switch ($invoice['status']) {
            case 'confirmed':
                // Also after a reorganisation put a paid order back on hold.
                if (!$paid || $current === $confirming) {
                    $this->setState($order, (int) Configuration::get('PS_OS_PAYMENT'));
                    // Show the xmrgate invoice as the payment's transaction ID.
                    foreach ($order->getOrderPaymentCollection() as $payment) {
                        if ($payment->transaction_id === '' || $payment->transaction_id === null) {
                            $payment->transaction_id = $invoice['id'];
                            $payment->update();
                        }
                    }
                }
                break;

            case 'seen':
                // While paid, "seen" means a confirmed payment lost its confirmations: a rare
                // chain reorganisation. Hold the order until it is confirmed again.
                if ($paid || in_array($current, [$awaiting, $underpaid, (int) Configuration::get('PS_OS_CANCELED')], true)) {
                    if ($current !== $confirming) {
                        $this->setState($order, $confirming);
                    }
                }
                break;

            case 'underpaid':
                if (!$paid && $current !== $underpaid) {
                    $this->setState($order, $underpaid);
                }
                break;

            case 'expired':
                if ($current === $awaiting) {
                    $this->setState($order, (int) Configuration::get('PS_OS_CANCELED'));
                }
                break;

            case 'pending':
                // A detected payment disappeared before being confirmed.
                if (!$paid && $current === $confirming) {
                    $this->setState($order, $awaiting);
                }
                break;
        }
    }

    private function setState(Order $order, $idState)
    {
        $history = new OrderHistory();
        $history->id_order = (int) $order->id;
        $history->changeIdOrderState((int) $idState, $order);
        $history->addWithemail(true);
    }
}
