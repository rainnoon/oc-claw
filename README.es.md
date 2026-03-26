<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <b>Español</b> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  Una mascota de escritorio en la barra de menú de macOS que monitorea tus agentes de programación con IA en tiempo real.
</p>

## Funciones

- **Mascota del Notch** — Un personaje vive junto al notch del MacBook, se anima cuando los agentes trabajan y duerme cuando están inactivos
- **Monitoreo de OpenClaw** — Detecta automáticamente agentes OpenClaw locales, muestra listas de sesiones, historial de chat y gráficos de llamadas/tokens diarios
- **Monitoreo de Claude Code** — Escucha sesiones locales de Claude Code mediante hooks, visualiza conversaciones en vivo
- **Modo Remoto** — Conéctate a instancias de OpenClaw en servidores remotos
- **Sistema de Personajes** — Animaciones GIF personalizadas, empareja diferentes agentes con diferentes personajes
- **Solo Barra de Menú** — Sin icono en el Dock, se ejecuta como app de bandeja en la barra de estado

## Instalación

> [!TIP]
> **Dile a tu agente de IA (OpenClaw / Claude Code):**
>
> ```
> Instala oc-claw: curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **O ejecútalo tú mismo en la Terminal:**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> También puedes descargar el DMG desde [Releases](https://github.com/rainnoon/oc-claw/releases). Después de instalar, ejecuta `xattr -cr /Applications/oc-claw.app` para evitar el Gatekeeper de macOS.

## Stack Tecnológico

- **Tauri v2** + **React** + **TypeScript**
- Backend en **Rust** para interacción con el sistema y comunicación API
- APIs nativas de macOS para posicionamiento del notch y gestión de ventanas

## Desarrollo

```bash
cd frontend
npm install
npx tauri dev
```

## Licencia

MIT

---

<p align="center">
  <sub>Creado originalmente en KAON Hackathon</sub>
</p>
