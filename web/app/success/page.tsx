export default function SuccessPage() {
  return (
    <main style={{ fontFamily: "monospace", maxWidth: 600, margin: "80px auto", padding: "0 24px" }}>
      <h1>License purchased.</h1>
      <p>Check your email — your license key is on the way.</p>
      <p style={{ marginTop: 32, color: "#666" }}>
        Once it arrives, activate it with:
      </p>
      <pre style={{ background: "#f4f4f4", padding: 16, borderRadius: 4 }}>
        finch license activate --key FINCH-...
      </pre>
      <p style={{ marginTop: 32 }}>
        <a href="https://github.com/darwin-finch/finch">Documentation</a>
      </p>
    </main>
  );
}
