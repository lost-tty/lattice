function Header() {
  const status = S.status.value;
  return html`
    <header>
      <h1>Lattice</h1>
      <div id="status">
        <span class="dot ${status.state}"></span>${status.text}
      </div>
    </header>
  `;
}
