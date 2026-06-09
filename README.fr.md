<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <a href="https://www.oc-claw.ai"><img src="https://img.shields.io/badge/Télécharger_depuis-oc--claw.ai-8A2BE2?style=for-the-badge" alt="Download" /></a>
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <b>Français</b>
</p>
<p align="center">
  Animal de bureau qui surveille vos agents de programmation IA, compatible macOS et Windows.
</p>

<p align="center">
  <b>Mode Code</b><br/>
  <sub>macOS : OpenClaw, Claude Code, Cursor, Codex, OpenCode, Gemini CLI, Hermes Agent</sub><br/>
  <sub>Windows : OpenClaw, Claude Code, Cursor, Codex, OpenCode, Gemini CLI, Hermes Agent (SSH distant)</sub>
</p>
<p align="center">
  <img src="https://github.com/user-attachments/assets/74b8bbf8-ddcf-4149-a91e-d18d5c24fec6" width="600" />
</p>
<p align="center">
  <b>Mode Animal</b>
</p>
<p align="center">
  <img src="https://github.com/user-attachments/assets/2a143250-174a-406e-8a43-fd30db7ce071" width="600" />
</p>

## Fonctionnalités

- Réagit en temps réel à l'activité des agents OpenClaw / Claude Code / Codex / Cursor / Gemini CLI / Hermes Agent (en cours, inactif, en attente)
- Un personnage vit sur votre bureau (macOS / Windows), s'anime quand les agents travaillent et dort quand ils sont inactifs
- **macOS** : survolez la zone de l'encoche pour afficher le panneau de détails de session
- Détecte automatiquement les agents OpenClaw locaux, affiche les listes de sessions, l'historique des conversations et les graphiques d'appels/tokens quotidiens
- Écoute les sessions locales de Claude Code, Codex, Cursor et Gemini CLI via des hooks, visualise les conversations en direct
- Statistiques d'utilisation des tokens Gemini CLI via la télémétrie locale
- Connexion aux instances OpenClaw / Hermes Agent sur des serveurs distants via SSH
- Animations personnalisées, associez différents agents à différents personnages
- Arrière-plans d'île personnalisables avec outil de recadrage
- Effets sonores de fin et d'attente

## Prérequis

- macOS ou Windows
- [OpenClaw](https://github.com/nicepkg/openclaw), [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), [Cursor](https://www.cursor.com), [Gemini CLI](https://github.com/google-gemini/gemini-cli) et/ou [Hermes Agent](https://github.com/NousResearch/hermes-agent) installé

## Comment ça marche

```
OpenClaw Agents ──→ Fichiers de session JSONL ──→ Sondage de santé ──→ État d'activité
Claude Code     ──→ Hooks ──→ Parseur d'événements ──→ État d'activité
Codex           ──→ Hooks ──→ Parseur d'événements ──→ État d'activité
Cursor          ──→ Hooks ──→ Parseur d'événements ──→ État d'activité
Gemini CLI      ──→ Hooks ──→ Parseur d'événements ──→ État d'activité
Hermes Agent    ──→ Plugin ──→ Parseur d'événements ──→ État d'activité
                                                            ↓
                     Sprites animés ← Machine à états ← Effets sonores
```

OC-Claw sonde les fichiers de session OpenClaw pour détecter l'activité des agents, et écoute Claude Code, Codex, Cursor, Gemini CLI et Hermes Agent via les hooks/plugins installés. Les états d'activité pilotent les animations de personnages sur l'île de l'encoche (macOS) ou la zone de la barre des tâches (Windows), avec un panneau extensible pour les détails de session, l'historique des conversations et les métriques.

## Stack Technique

- **Tauri v2** + **React** + **TypeScript** — frontend
- **Rust** — backend pour l'interaction système, le tunneling SSH et la communication API
- APIs natives macOS / Windows pour la gestion des fenêtres

## Développement

```bash
cd frontend
npm install
npx tauri dev
```

## Contribuer

Les rapports de bugs, suggestions de fonctionnalités et pull requests sont les bienvenus.

## Lien d'amitié

Merci pour le soutien et les retours des amis de [LINUX DO](https://linux.do/).

## Crédits

- [Notchi](https://github.com/sk-ruban/notchi) — inspiration de design pour le concept de compagnon d'encoche et l'île herbeuse
- [Vibe Island](https://github.com/vibeislandapp/vibe-island) — référence de design d'interaction

## Licence

MIT

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=rainnoon/oc-claw&type=Date)](https://star-history.com/#rainnoon/oc-claw&Date)

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <sub>Créé à l'origine lors du KAON Hackathon</sub>
</p>
