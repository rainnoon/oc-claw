<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <a href="https://www.oc-claw.ai"><img src="https://img.shields.io/badge/Descargar_desde-oc--claw.ai-8A2BE2?style=for-the-badge" alt="Download" /></a>
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <b>Español</b> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  Mascota de escritorio que monitorea agentes de programación con IA, compatible con macOS y Windows.
</p>

<p align="center">
  <img src="https://github.com/user-attachments/assets/71b1518d-d611-4b86-ba06-d78c719995db" width="600" />
</p>

## Funciones

- Reacciona en tiempo real a la actividad de agentes OpenClaw / Claude Code (trabajando, inactivo, esperando)
- Un personaje vive en tu escritorio (macOS / Windows), se anima cuando los agentes trabajan y duerme cuando están inactivos
- Detecta automáticamente agentes OpenClaw locales, muestra listas de sesiones, historial de chat y gráficos de llamadas/tokens diarios
- Escucha sesiones locales de Claude Code mediante hooks, visualiza conversaciones en vivo
- Conecta a instancias de OpenClaw en servidores remotos vía SSH
- Animaciones personalizadas, empareja diferentes agentes con diferentes personajes
- Fondos de isla personalizables con herramienta de recorte
- Efectos de sonido de finalización y espera

## Requisitos

- macOS o Windows
- [OpenClaw](https://github.com/nicepkg/openclaw) y/o [Claude Code](https://docs.anthropic.com/en/docs/claude-code) instalado

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
- APIs nativas de macOS / Windows para gestión de ventanas

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
- [Vibe Island](https://github.com/vibeislandapp/vibe-island) — referencia de diseño de interacción

## Licencia

MIT

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <sub>Creado originalmente en KAON Hackathon</sub>
</p>
