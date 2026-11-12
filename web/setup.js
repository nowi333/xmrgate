(function () {
  const t = xg.translator({
    en: {
      title: "Accept Monero in your shop",
      subtitle: "Two things from your wallet and you are done. No node to install, nothing to sync.",
      code_label: "Setup code",
      code_hint: "Shown in the xmrgate window or logs when it starts.",
      address_label: "Your Monero address",
      address_hint: "The main address of your wallet. It starts with a 4. Payments go straight to it.",
      key_label: "Secret view key",
      key_hint: "In your wallet, look under Settings, then “Seed & keys”. This key can only see incoming payments. It cannot spend anything, and it never leaves this server.",
      show: "Show",
      hide: "Hide",
      shop_label: "Shop name (optional)",
      shop_hint: "Shown to your customers on the payment page.",
      password_label: "Choose a password",
      password_hint: "At least 8 characters. It protects this dashboard.",
      submit: "Start accepting Monero",
      working: "Checking…",
      safety: "Never type your 25-word seed or your spend key here. xmrgate does not need them and can never move your funds.",
      match_mainnet: "Address and key match. You are ready.",
      match_test: "Address and key match. This is a {network} wallet, for testing only: it does not hold real money.",
      invalid_address: "This does not look like a Monero address. Copy it again from your wallet.",
      not_primary_address: "This is a subaddress or an integrated address. Use your wallet's main address, the one starting with a 4.",
      invalid_view_key: "This is not a valid view key. It is 64 characters long, using digits and the letters a to f.",
      view_key_mismatch: "This view key belongs to another wallet. Copy both the address and the key from the same wallet.",
      weak_password: "The password needs at least 8 characters.",
      bad_token: "Wrong setup code. Copy it from the xmrgate window or logs.",
      already_configured: "Setup is already done. Sign in instead.",
      generic: "Something went wrong. Try again.",
    },
  });
  t.apply();
  document.title = t("title");

  const el = xg.el;
  const code = new URLSearchParams(location.search).get("code") || "";
  if (!code) el("code-row").classList.remove("hidden");

  el("toggle_key").addEventListener("click", () => {
    const hidden = el("view_key").type === "password";
    el("view_key").type = hidden ? "text" : "password";
    el("toggle_key").textContent = t(hidden ? "hide" : "show");
  });

  function note(kind, text) {
    const box = el("wallet_note");
    box.className = "note " + kind;
    box.textContent = text;
  }

  let checkTimer;
  function checkWallet() {
    clearTimeout(checkTimer);
    const address = el("address").value.trim();
    const view_key = el("view_key").value.trim();
    if (!address || !view_key) {
      el("wallet_note").classList.add("hidden");
      return;
    }
    checkTimer = setTimeout(async () => {
      const { data } = await xg.post("/setup/api/check", { address, view_key });
      if (address !== el("address").value.trim() || view_key !== el("view_key").value.trim()) return;
      if (data.ok) {
        note("ok", data.network === "mainnet" ? t("match_mainnet") : t("match_test", { network: data.network }));
      } else {
        note("bad", t(data.error || "generic"));
      }
    }, 300);
  }
  el("address").addEventListener("input", checkWallet);
  el("view_key").addEventListener("input", checkWallet);

  el("form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const error = el("error");
    error.classList.add("hidden");
    const button = el("submit");
    button.disabled = true;
    button.textContent = t("working");
    const { ok, data } = await xg.post("/setup/api", {
      token: code || el("code").value.trim(),
      address: el("address").value,
      view_key: el("view_key").value,
      shop_name: el("shop_name").value,
      password: el("password").value,
    });
    if (ok) {
      location.href = "/admin";
      return;
    }
    button.disabled = false;
    button.textContent = t("submit");
    const known = ["invalid_address", "not_primary_address", "invalid_view_key", "view_key_mismatch", "weak_password", "bad_token", "already_configured"];
    error.textContent = t(known.includes(data.error) ? data.error : "generic");
    error.classList.remove("hidden");
    error.scrollIntoView({ block: "center" });
  });
})();
