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
  Un compañero del notch de macOS que monitorea tus agentes de programación con IA en tiempo real.
</p>

## Funciones

- Reacciona en tiempo real a la actividad de agentes OpenClaw / Claude Code (trabajando, inactivo, esperando)
- Un personaje vive junto al notch del MacBook, se anima cuando los agentes trabajan y duerme cuando están inactivos
- Detecta automáticamente agentes OpenClaw locales, muestra listas de sesiones, historial de chat y gráficos de llamadas/tokens diarios
- Escucha sesiones locales de Claude Code mediante hooks, visualiza conversaciones en vivo
- Conecta a instancias de OpenClaw en servidores remotos vía SSH
- Animaciones GIF personalizadas, empareja diferentes agentes con diferentes personajes
- Fondos de isla personalizables con herramienta de recorte
- Efectos de sonido de finalización y espera
- Solo barra de menú — sin icono en el Dock

## Requisitos

- macOS (MacBook con notch recomendado)
- [OpenClaw](https://github.com/nicepkg/openclaw) y/o [Claude Code](https://docs.anthropic.com/en/docs/claude-code) instalado

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

## Cómo funciona

```
OpenClaw Agents ──→ Archivos de sesión JSONL ──→ Sondeo de salud ──→ Estado de actividad
Claude Code     ──→ Hooks (SessionStart/Stop) ──→ Parser de eventos ──→ Estado de actividad
                                                                              ↓
                                          Sprites animados ← Máquina de estados ← Efectos de sonido
```

OC-Claw sondea los archivos de sesión de OpenClaw para detectar actividad de agentes, y escucha Claude Code mediante hooks instalados. Los estados de actividad impulsan las animaciones de personajes en la isla del notch, con un panel expandible para detalles de sesión, historial de chat y métricas.

## Stack Tecnológico

- **Tauri v2** + **React** + **TypeScript** — frontend
- **Rust** — backend para interacción con el sistema, túneles SSH y comunicación API
- APIs nativas de macOS para posicionamiento del notch y gestión de ventanas

## Desarrollo

```bash
cd frontend
npm install
npx tauri dev
```

## Contribuir

Se aceptan reportes de errores, sugerencias de funciones y pull requests.

## Créditos

- [Notchi](https://github.com/sk-ruban/notchi) — inspiración de diseño para el concepto de compañero del notch y la isla de césped
- [OpenClaw](https://github.com/nicepkg/openclaw) — la plataforma de agentes IA que esta app monitorea

## Licencia

MIT

---

<p align="center">
  <sub>Creado originalmente en KAON Hackathon</sub>
</p>
