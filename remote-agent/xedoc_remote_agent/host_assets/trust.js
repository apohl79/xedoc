(async () => {
  const output = document.getElementById("ca-fingerprint");
  try {
    const response = await fetch("/api/ca/fingerprint");
    if (!response.ok) throw new Error("This host has no managed CA certificate.");
    output.textContent = (await response.json()).sha256;
  } catch (error) {
    output.textContent = error.message;
  }
})();
