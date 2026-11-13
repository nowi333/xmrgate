<?php
/**
 * Receive a payment update from the gateway.
 *
 * The body is signed with the secret key. The invoice is then read back from the gateway, so
 * the order only ever changes on the gateway's own current answer.
 *
 * @license MIT
 */

if (!defined('_PS_VERSION_')) {
    exit;
}

class XmrgateWebhookModuleFrontController extends ModuleFrontController
{
    public function postProcess()
    {
        /** @var Xmrgate $module */
        $module = $this->module;

        $body = (string) file_get_contents('php://input');
        $signature = isset($_SERVER['HTTP_X_XMRGATE_SIGNATURE']) ? (string) $_SERVER['HTTP_X_XMRGATE_SIGNATURE'] : '';
        $expected = hash_hmac('sha256', $body, $module->apiKey());
        if ($module->apiKey() === '' || $signature === '' || !hash_equals($expected, $signature)) {
            $this->finish(401);
        }

        $payload = json_decode($body, true);
        $invoiceId = isset($payload['invoice']['id']) ? (string) $payload['invoice']['id'] : '';
        if ($invoiceId === '' || !preg_match('/^[A-Za-z0-9_-]{1,64}$/', $invoiceId)) {
            $this->finish(400);
        }

        $order = new Order($module->orderIdForInvoice($invoiceId));
        if (!Validate::isLoadedObject($order)) {
            // Not one of ours: acknowledge so the gateway stops retrying.
            $this->finish(200);
        }

        $response = $module->request('GET', '/api/v1/invoices/' . rawurlencode($invoiceId));
        if (isset($response['error']) || $response['code'] !== 200 || empty($response['body']['status'])) {
            // Ask the gateway to try again later.
            $this->finish(503);
        }

        $module->applyStatus($order, $response['body']);
        $this->finish(200);
    }

    private function finish($code)
    {
        http_response_code($code);
        header('Content-Type: text/plain');
        exit;
    }
}
