import "./styles.css";

const app = document.querySelector<HTMLDivElement>("#app")!;

if (app) {
  app.innerHTML = `
    <div class="panel">
      <div class="placeholder">ClipVault &middot; U1 scaffold</div>
    </div>
  `;
}
