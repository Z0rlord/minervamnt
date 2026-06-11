(function () {
  "use strict";

  var TOS_VERSION = "2026-06-11-v1";
  var STORAGE_KEY = "minervamnt_tos_accepted";

  function getAccepted() {
    try {
      var raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) return null;
      return JSON.parse(raw);
    } catch (_e) {
      return null;
    }
  }

  function setAccepted() {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        version: TOS_VERSION,
        acceptedAt: new Date().toISOString(),
      })
    );
  }

  function unlockSite() {
    document.body.classList.remove("tos-locked");
    var gate = document.getElementById("tos-gate");
    var content = document.getElementById("site-content");
    if (gate) gate.hidden = true;
    if (content) content.hidden = false;
  }

  function initGate() {
    var accepted = getAccepted();
    if (accepted && accepted.version === TOS_VERSION) {
      unlockSite();
      return;
    }

    document.body.classList.add("tos-locked");
    var gate = document.getElementById("tos-gate");
    var content = document.getElementById("site-content");
    var checkbox = document.getElementById("tos-agree");
    var continueBtn = document.getElementById("tos-continue");
    var declineBtn = document.getElementById("tos-decline");
    var msg = document.getElementById("tos-decline-msg");

    if (gate) gate.hidden = false;
    if (content) content.hidden = true;

    function syncButton() {
      if (continueBtn && checkbox) {
        continueBtn.disabled = !checkbox.checked;
      }
    }

    if (checkbox) {
      checkbox.addEventListener("change", syncButton);
      syncButton();
    }

    if (continueBtn) {
      continueBtn.addEventListener("click", function () {
        if (!checkbox || !checkbox.checked) return;
        setAccepted();
        unlockSite();
      });
    }

    if (declineBtn) {
      declineBtn.addEventListener("click", function () {
        if (msg) {
          msg.textContent =
            "You must accept the Terms of Service to use this site.";
        }
      });
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initGate);
  } else {
    initGate();
  }
})();
