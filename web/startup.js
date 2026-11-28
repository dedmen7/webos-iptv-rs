
async function startOrbis(encodedModule) {
  if (typeof WebAssembly === "undefined") return;

  try {
    const binary = atob(encodedModule);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < bytes.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    await wasm_bindgen({ module_or_path: bytes });
  } catch (error) {
    console.error("Orbis IPTV startup", error);
    const system = window.webOSSystem || window.PalmSystem || {};
    const lang = (system.locale || navigator.language || "en").split(/[-_]/)[0].toLowerCase();
    const messages = {
      en: "Orbis IPTV could not start.",
      nl: "Orbis IPTV kon niet starten.",
      de: "Orbis IPTV konnte nicht gestartet werden.",
      fr: "Orbis IPTV n’a pas pu démarrer.",
    };
    document.getElementById("initial").setAttribute("data-error", "");
    document.getElementById("initial-message").textContent = messages[lang] || messages.en;
  }
}
