<?php
/**
 * Where the gateway sends the customer back after paying: the shop's order confirmation page.
 *
 * @license MIT
 */

if (!defined('_PS_VERSION_')) {
    exit;
}

class XmrgateReturnModuleFrontController extends ModuleFrontController
{
    public function postProcess()
    {
        $idCart = (int) Tools::getValue('id_cart');
        $key = (string) Tools::getValue('key');
        $idOrder = (int) Order::getIdByCartId($idCart);
        $order = new Order($idOrder);

        if (!Validate::isLoadedObject($order) || $order->module !== $this->module->name || $order->secure_key !== $key) {
            Tools::redirect($this->context->link->getPageLink('index', true));
        }

        Tools::redirect($this->context->link->getPageLink('order-confirmation', true, null, [
            'id_cart' => $idCart,
            'id_module' => (int) $this->module->id,
            'id_order' => $idOrder,
            'key' => $key,
        ]));
    }
}
