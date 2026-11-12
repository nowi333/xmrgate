(function () {
  const t = xg.translator({
    en: {
      title: "Sign in",
      password: "Password",
      submit: "Sign in",
      wrong_password: "Wrong password.",
      locked: "Too many attempts. Wait a minute and try again.",
      generic: "Something went wrong. Try again.",
    },
  });
  t.apply();
  xg.el("form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const { ok, data } = await xg.post("/login/api", { password: xg.el("password").value });
    if (ok) {
      location.href = "/admin";
      return;
    }
    const error = xg.el("error");
    error.textContent = t(["wrong_password", "locked"].includes(data.error) ? data.error : "generic");
    error.classList.remove("hidden");
  });
})();
