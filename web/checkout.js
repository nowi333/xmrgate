(function () {
  const t = xg.translator({
    en: {
      page_title: "Pay with Monero",
      pay_title: "Send exactly",
      pay_rest: "Send the remaining",
      tap_hint: "Scan the code with your Monero wallet, or tap the amount or the address to copy it.",
      open_wallet: "Open in my wallet",
      waiting: "Waiting for your payment…",
      expires_in: "This request expires in {time}.",
      partial: "We received {received} XMR. {due} XMR is still missing.",
      copied_amount: "Amount copied",
      copied_address: "Address copied",
      seen_title: "Payment received",
      seen_text: "Your payment is on its way. We are waiting for the network to confirm it, which takes a few minutes.",
      seen_count: "Confirmations: {have} of {need}",
      seen_close: "You can close this page. The shop is notified automatically.",
      done_title: "Payment complete",
      done_text: "Thank you. Your payment is confirmed.",
      back: "Return to the shop",
      expired_title: "This payment request has expired",
      expired_text: "Please do not send anything to this address. Go back to the shop to start again.",
      order: "Order {ref}",
      footer: "Paid directly to the shop. No intermediary holds your money.",
      qr_alt: "QR code for this payment",
      shop_fallback: "Monero payment",
    },
  });
  t.apply();
  document.title = t("page_title");

  const el = xg.el;
  const base = location.pathname.replace(/\/$/, "");
  let invoice = null;
  let clockOffset = 0;
  let qrFor = "";
  let redirecting = false;

  function show(view) {
    for (const name of ["pay", "seen", "confirmed", "expired"]) {
      el("view-" + name).classList.toggle("hidden", name !== view);
    }
  }

  function backLink(id) {
    const link = el(id);
    if (invoice.redirect_url) {
      link.href = invoice.redirect_url;
      link.classList.remove("hidden");
    }
  }

  function countdown() {
    if (!invoice || !["pending", "underpaid"].includes(invoice.status)) return;
    const left = invoice.expires_at - (Math.floor(Date.now() / 1000) + clockOffset);
    if (left <= 0) {
      el("countdown").textContent = "";
      return;
    }
    const h = Math.floor(left / 3600);
    const m = Math.floor((left % 3600) / 60);
    const s = left % 60;
    const pad = (n) => String(n).padStart(2, "0");
    el("countdown").textContent = t("expires_in", { time: (h ? h + ":" + pad(m) : m) + ":" + pad(s) });
  }

  function render() {
    el("shop").textContent = invoice.shop_name || t("shop_fallback");
    el("order").textContent = invoice.order_ref ? t("order", { ref: invoice.order_ref }) : (invoice.description || "");

    if (invoice.status === "confirmed") {
      show("confirmed");
      backLink("back-done");
      if (invoice.redirect_url && !redirecting) {
        redirecting = true;
        setTimeout(() => { location.href = invoice.redirect_url; }, 4000);
      }
      return;
    }
    if (invoice.status === "seen") {
      show("seen");
      el("seen-text").textContent = t("seen_text");
      const need = Math.max(invoice.confirmations_required, 1);
      el("seen-count").textContent = t("seen_count", { have: invoice.confirmations, need: invoice.confirmations_required });
      el("bar").style.width = Math.min(100, Math.round((invoice.confirmations / need) * 100)) + "%";
      return;
    }
    if (invoice.status === "expired") {
      show("expired");
      backLink("back-expired");
      return;
    }

    show("pay");
    const partial = invoice.status === "underpaid";
    const due = partial ? invoice.due_xmr : invoice.amount_xmr;
    el("pay-title").textContent = t(partial ? "pay_rest" : "pay_title");
    el("amount").textContent = "";
    el("amount").append(due + " ");
    const unit = document.createElement("small");
    unit.textContent = "XMR";
    el("amount").append(unit);
    el("fiat").textContent = invoice.fiat_amount && !partial ? "≈ " + invoice.fiat_amount + " " + invoice.fiat_currency : "";
    el("partial").classList.toggle("hidden", !partial);
    el("partial").textContent = partial ? t("partial", { received: invoice.received_xmr, due: invoice.due_xmr }) : "";
    el("address").textContent = invoice.address;
    el("open").href = invoice.payment_uri;
    if (qrFor !== invoice.payment_uri) {
      qrFor = invoice.payment_uri;
      el("qr").src = base + "/qr.svg?v=" + encodeURIComponent(due);
      el("qr").alt = t("qr_alt");
    }
    countdown();
  }

  el("amount").addEventListener("click", () => {
    const due = invoice.status === "underpaid" ? invoice.due_xmr : invoice.amount_xmr;
    xg.copy(due, t("copied_amount"));
  });
  el("address").addEventListener("click", () => xg.copy(invoice.address, t("copied_address")));

  async function poll() {
    try {
      const { ok, data } = await xg.get(base + "/status");
      if (ok) {
        invoice = data;
        clockOffset = data.now - Math.floor(Date.now() / 1000);
        render();
      }
    } catch (e) {
      // Network hiccup: keep the last known state and try again.
    }
    const settled = invoice && (invoice.status === "confirmed" || invoice.status === "expired");
    setTimeout(poll, settled ? 30000 : 3000);
  }
  poll();
  setInterval(countdown, 1000);
})();
