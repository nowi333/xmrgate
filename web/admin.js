(function () {
  const t = xg.translator({
    en: {
      logout: "Sign out",
      state_ready: "Ready",
      state_syncing: "Catching up",
      state_problem: "Needs attention",
      state_starting: "Starting",
      stat_paid: "payments received",
      stat_received: "XMR received",
      stat_height: "last block checked",
      stat_height_idle: "network height (resting: no payment awaited)",
      connect_title: "Connect a website",
      connect_text: "Shops and websites create payments through this gateway. Each one gets its own secret key.",
      gateway_label: "Gateway address",
      sites_title: "Your websites",
      site_placeholder: "Name of another website",
      site_add: "Add website",
      site_key: "Secret key of {name}",
      site_rotate: "Replace key",
      site_rotate_confirm: "The current key of “{name}” stops working at once. You will need to paste the new one into that website. Continue?",
      site_remove_confirm: "“{name}” will no longer be able to create payments. Its past payments are kept. Continue?",
      how_title: "How to connect",
      how_woo: "Install the plugin, then paste the gateway address and the website's secret key under WooCommerce → Settings → Payments → Monero.",
      download: "Download the WooCommerce plugin",
      how_any_title: "Any other website.",
      how_any: "A few lines of code in any language: create a payment, send the customer to it, get called back when it is paid.",
      how_any_link: "Open the developer guide",
      how_nocode_title: "No code at all.",
      how_nocode: "Use a payment button, below.",
      buttons_title: "Payment buttons",
      buttons_text: "A permanent link with a fixed price. Paste it on any page, in an email or on a social profile. No code needed.",
      button_name: "What are you selling?",
      price: "Price",
      button_redirect: "Page to show after payment (optional)",
      button_create: "Create payment button",
      copy_link: "Copy link",
      copy_html: "Copy HTML",
      button_remove_confirm: "The link of “{name}” will stop working. Continue?",
      invalid_name: "Give it a name.",
      invalid_url: "The page address must start with https://",
      copy: "Copy",
      copied: "Copied",
      show: "Show",
      hide: "Hide",
      open: "Open",
      add: "Add",
      remove: "Remove",
      save: "Save",
      saved: "Saved.",
      link_title: "Request a payment",
      link_text: "A one-time payment link for a single customer: an invoice, a quote, a custom order.",
      amount: "Amount",
      description: "What is it for? (optional)",
      link_create: "Create payment link",
      payments_title: "Recent payments",
      payments_empty: "No payment yet. They will show up here as soon as a customer pays.",
      col_date: "Date",
      col_ref: "Reference",
      col_amount: "Amount",
      col_status: "Status",
      view: "View",
      status_pending: "Waiting",
      status_underpaid: "Partly paid",
      status_seen: "Confirming",
      status_confirmed: "Paid",
      status_expired: "Expired",
      settings_title: "Settings",
      set_shop: "Shop name",
      set_conf: "Confirmations before a payment counts as paid",
      set_conf_hint: "1 is safe for everyday amounts and takes about 2 minutes. Use 10 for large amounts. 0 accepts payments instantly but can be cheated.",
      set_expiry: "Minutes a customer has to pay",
      set_url: "Public address of this gateway",
      set_url_hint: "Leave empty unless customers reach this server through a different address than you do.",
      set_nodes: "Monero nodes",
      set_nodes_hint: "xmrgate reads the public blockchain from these servers and checks them against each other. They never see your key or your payments. Keep at least two.",
      set_tor: "Tor proxy (optional)",
      set_tor_hint: "Hides this server's address from the nodes. Needs Tor running on this machine.",
      wallet_label: "Payments go to:",
      events_title: "Activity log",
      events_empty: "Nothing to report.",
      node_ok: "Works. Height {height}, answered in {ms} ms. Press Save to keep it.",
      node_bad: "This node did not answer correctly: {message}",
      node_testing: "Testing…",
      node_height: "block {height}",
      node_unknown: "not checked yet",
      alert_no_nodes: "No Monero node is configured, so payments cannot be detected. Add at least two nodes in Settings.",
      alert_few_nodes: "Fewer than two nodes are answering, so payments cannot be verified right now. Add working nodes in Settings.",
      alert_single_node: "Only one node is configured. Add a second one in Settings so they can be checked against each other.",
      alert_scan_error: "The scanner hit a problem and is retrying: {error}",
      alert_upgrade_required: "The Monero network was upgraded and this version of xmrgate cannot read it. Update xmrgate now: payments are not being detected.",
      alert_behind: "xmrgate is catching up with the network. Recent payments may take a little longer to appear.",
      invalid_amount: "Enter a valid amount, such as 19.99.",
      rate_unavailable: "The exchange rate could not be fetched. Try again, or enter the amount in XMR.",
      generic: "Something went wrong. Try again.",
    },
  });
  t.apply();

  const el = xg.el;
  let state = null;
  let nodes = [];
  let settingsLoaded = false;

  el("logout").addEventListener("click", async () => {
    await xg.post("/logout");
    location.href = "/login";
  });

  function node(tag, className, text) {
    const item = document.createElement(tag);
    if (className) item.className = className;
    if (text !== undefined) item.textContent = text;
    return item;
  }

  function when(seconds) {
    return new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "short" }).format(new Date(seconds * 1000));
  }

  function renderNodes() {
    const box = el("nodes");
    box.textContent = "";
    const status = {};
    for (const entry of (state && state.nodes) || []) status[entry.url] = entry;
    for (const url of nodes) {
      const info = status[url];
      const row = node("div", "node");
      row.append(node("span", "lamp" + (info ? (info.healthy ? " on" : " off") : "")));
      row.append(node("span", "url mono", url));
      const detail = info
        ? (info.healthy ? t("node_height", { height: info.height }) : (info.error || ""))
        : t("node_unknown");
      row.append(node("span", "muted small", detail));
      const remove = node("button", "link", t("remove"));
      remove.type = "button";
      remove.addEventListener("click", () => {
        nodes = nodes.filter((known) => known !== url);
        renderNodes();
      });
      row.append(remove);
      box.append(row);
    }
  }

  const shownKeys = new Set();

  function renderSites() {
    const box = el("sites");
    // Do not rebuild while someone is selecting a key.
    if (box.contains(document.activeElement)) return;
    box.textContent = "";
    for (const site of state.sites) {
      const row = node("div", "row");
      row.append(node("div", "grow", site.name));
      const field = node("div", "copyfield");
      const input = node("input", "site-key");
      input.readOnly = true;
      input.type = shownKeys.has(site.id) ? "text" : "password";
      input.value = site.api_key;
      input.setAttribute("aria-label", t("site_key", { name: site.name }));
      const show = node("button", "", t(shownKeys.has(site.id) ? "hide" : "show"));
      show.type = "button";
      show.addEventListener("click", () => {
        if (shownKeys.has(site.id)) shownKeys.delete(site.id); else shownKeys.add(site.id);
        document.activeElement.blur();
        renderSites();
      });
      const copy = node("button", "", t("copy"));
      copy.type = "button";
      copy.addEventListener("click", () => xg.copy(site.api_key, t("copied")));
      field.append(input, show, copy);
      row.append(field);
      const rotate = node("button", "link", t("site_rotate"));
      rotate.type = "button";
      rotate.addEventListener("click", async () => {
        if (!confirm(t("site_rotate_confirm", { name: site.name }))) return;
        await xg.post("/admin/api/sites/" + site.id + "/rotate-key");
        document.activeElement.blur();
        refresh();
      });
      const remove = node("button", "link", t("remove"));
      remove.type = "button";
      remove.addEventListener("click", async () => {
        if (!confirm(t("site_remove_confirm", { name: site.name }))) return;
        await xg.post("/admin/api/sites/" + site.id + "/delete");
        document.activeElement.blur();
        refresh();
      });
      row.append(rotate, remove);
      box.append(row);
    }
  }

  function renderButtons() {
    const box = el("buttons");
    box.textContent = "";
    for (const button of state.buttons) {
      const row = node("div", "row");
      const label = node("div", "grow", button.name);
      label.append(node("div", "muted small", button.amount + " " + button.currency));
      row.append(label);
      const link = node("button", "", t("copy_link"));
      link.type = "button";
      link.addEventListener("click", () => xg.copy(button.url, t("copied")));
      const html = node("button", "", t("copy_html"));
      html.type = "button";
      html.addEventListener("click", () => {
        const anchor = document.createElement("a");
        anchor.href = button.url;
        anchor.textContent = "Pay with Monero";
        xg.copy(anchor.outerHTML, t("copied"));
      });
      const open = node("a", "button", t("open"));
      open.href = button.url;
      open.target = "_blank";
      open.rel = "noopener";
      const remove = node("button", "link", t("remove"));
      remove.type = "button";
      remove.addEventListener("click", async () => {
        if (!confirm(t("button_remove_confirm", { name: button.name }))) return;
        await xg.post("/admin/api/buttons/" + button.id + "/delete");
        refresh();
      });
      row.append(link, html, open, remove);
      box.append(row);
    }
  }

  function render() {
    const health = state.health;
    const alerts = state.alerts;
    const blocking = alerts.some((a) => ["no_nodes", "few_nodes", "scan_error", "upgrade_required"].includes(a));
    const badge = el("state");
    if (blocking) {
      badge.className = "badge bad";
      badge.textContent = t("state_problem");
    } else if (alerts.includes("behind")) {
      badge.className = "badge seen";
      badge.textContent = t("state_syncing");
    } else if (health.last_ok_at) {
      badge.className = "badge confirmed";
      badge.textContent = t("state_ready");
    } else {
      badge.className = "badge pending";
      badge.textContent = t("state_starting");
    }

    el("shop").textContent = state.settings.shop_name || "xmrgate";
    document.title = (state.settings.shop_name ? state.settings.shop_name + " · " : "") + "xmrgate";

    const box = el("alerts");
    box.textContent = "";
    for (const alert of alerts) {
      const kind = ["single_node", "behind"].includes(alert) ? "warn" : "bad";
      box.append(node("div", "note " + kind, t("alert_" + alert, { error: health.last_error || "" })));
    }
    if (alerts.length) box.append(node("p"));

    el("stat-paid").textContent = state.totals.paid;
    el("stat-received").textContent = state.totals.received_xmr;
    el("stat-height").textContent = health.scanned != null ? health.scanned.toLocaleString(undefined) : "–";
    el("stat-height-label").textContent = t(health.idle ? "stat_height_idle" : "stat_height");

    el("gateway-url").value = state.gateway_url;
    el("wallet").textContent = state.primary_address;
    renderSites();
    renderButtons();

    const rows = el("payments");
    rows.textContent = "";
    for (const invoice of state.invoices) {
      const row = node("tr");
      row.append(node("td", "num", when(invoice.created_at)));
      const reference = node("td", "", invoice.order_ref || invoice.description || "–");
      if (invoice.source && invoice.source !== reference.textContent) {
        reference.append(node("div", "muted small", invoice.source));
      }
      row.append(reference);
      const amount = node("td", "num", invoice.amount_xmr + " XMR");
      if (invoice.fiat_amount) {
        amount.append(node("div", "muted small", invoice.fiat_amount + " " + invoice.fiat_currency));
      }
      row.append(amount);
      const status = node("td");
      status.append(node("span", "badge " + invoice.status, t("status_" + invoice.status)));
      row.append(status);
      const link = node("td");
      const anchor = node("a", "", t("view"));
      anchor.href = invoice.checkout_url;
      anchor.target = "_blank";
      anchor.rel = "noopener";
      link.append(anchor);
      row.append(link);
      rows.append(row);
    }
    el("no-payments").classList.toggle("hidden", state.invoices.length > 0);
    el("payments-wrap").classList.toggle("hidden", state.invoices.length === 0);

    const events = el("events");
    events.textContent = "";
    if (!state.events.length) events.append(node("li", "muted", t("events_empty")));
    for (const event of state.events) {
      const item = node("li");
      item.append(node("span", "muted", when(event.at) + " · "));
      item.append(event.message);
      events.append(item);
    }

    if (!settingsLoaded) {
      settingsLoaded = true;
      el("set-shop").value = state.settings.shop_name;
      el("set-conf").value = state.settings.confirmations;
      el("set-expiry").value = state.settings.expiry_minutes;
      el("set-url").value = state.settings.public_url;
      el("set-tor").value = state.settings.tor_proxy;
      nodes = state.settings.nodes.slice();
    }
    renderNodes();
  }

  async function refresh() {
    try {
      const { ok, status, data } = await xg.get("/admin/api/state");
      if (status === 401) {
        location.href = "/login";
        return;
      }
      if (ok) {
        state = data;
        render();
      }
    } catch (e) {
      // Keep showing the last known state.
    }
  }

  el("copy-url").addEventListener("click", () => xg.copy(el("gateway-url").value, t("copied")));
  el("site-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const { ok } = await xg.post("/admin/api/sites", { name: el("site-name").value });
    if (ok) el("site-name").value = "";
    document.activeElement.blur();
    refresh();
  });

  el("button-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    el("button-error").classList.add("hidden");
    const { ok, data } = await xg.post("/admin/api/buttons", {
      name: el("button-name").value,
      amount: el("button-amount").value.replace(",", "."),
      currency: el("button-currency").value,
      redirect_url: el("button-redirect").value || undefined,
    });
    if (!ok) {
      const known = ["invalid_amount", "invalid_name", "invalid_url"];
      el("button-error").textContent = t(known.includes(data.error) ? data.error : "generic");
      el("button-error").classList.remove("hidden");
      return;
    }
    el("button-name").value = "";
    el("button-amount").value = "";
    el("button-redirect").value = "";
    refresh();
  });

  el("link-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    el("link-error").classList.add("hidden");
    const { ok, data } = await xg.post("/admin/api/invoices", {
      amount: el("link-amount").value.replace(",", "."),
      currency: el("link-currency").value,
      description: el("link-description").value || undefined,
    });
    if (!ok) {
      const known = ["invalid_amount", "rate_unavailable"];
      el("link-error").textContent = t(known.includes(data.error) ? data.error : "generic");
      el("link-error").classList.remove("hidden");
      return;
    }
    el("link-url").value = data.checkout_url;
    el("link-open").href = data.checkout_url;
    el("link-result").classList.remove("hidden");
    el("link-amount").value = "";
    el("link-description").value = "";
    refresh();
  });
  el("link-copy").addEventListener("click", () => xg.copy(el("link-url").value, t("copied")));

  el("node-add").addEventListener("click", async () => {
    const url = el("node-url").value.trim().replace(/\/+$/, "");
    if (!url) return;
    const note = el("node-note");
    note.className = "note info";
    note.textContent = t("node_testing");
    const { data } = await xg.post("/admin/api/test-node", { url });
    if (data.ok) {
      note.className = "note ok";
      note.textContent = t("node_ok", { height: data.height, ms: data.latency_ms });
      if (!nodes.includes(url)) nodes.push(url);
      el("node-url").value = "";
      renderNodes();
    } else {
      note.className = "note bad";
      note.textContent = t("node_bad", { message: data.message || "" });
    }
  });

  el("settings-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    el("settings-error").classList.add("hidden");
    el("settings-saved").textContent = "";
    const { ok, data } = await xg.post("/admin/api/settings", {
      shop_name: el("set-shop").value,
      confirmations: Number(el("set-conf").value),
      expiry_minutes: Number(el("set-expiry").value),
      public_url: el("set-url").value,
      tor_proxy: el("set-tor").value,
      nodes,
    });
    if (!ok) {
      el("settings-error").textContent = data.message || t("generic");
      el("settings-error").classList.remove("hidden");
      return;
    }
    el("settings-saved").textContent = t("saved");
    refresh();
  });

  refresh();
  setInterval(refresh, 5000);
})();
