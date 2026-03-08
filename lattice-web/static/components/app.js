function App() {
  return html`
    <${Header} />
    <div id="main">
      <${Sidebar} />
      <div id="content">
        <${TabBar} />
        <${Panel} />
      </div>
    </div>
    <${Toasts} />
    <${ModalContainer} />
  `;
}
