<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <b>Français</b>
</p>
<p align="center">
  Un animal de compagnie de bureau dans la barre de menus macOS qui surveille vos agents de programmation IA en temps réel.
</p>

## Fonctionnalités

- **Animal du Notch** — Un personnage vit à côté de l'encoche du MacBook, s'anime quand les agents travaillent et dort quand ils sont inactifs
- **Surveillance OpenClaw** — Détecte automatiquement les agents OpenClaw locaux, affiche les listes de sessions, l'historique des conversations et les graphiques d'appels/tokens quotidiens
- **Surveillance Claude Code** — Écoute les sessions locales de Claude Code via des hooks, visualise les conversations en direct
- **Mode Distant** — Connectez-vous aux instances OpenClaw sur des serveurs distants
- **Système de Personnages** — Animations GIF personnalisées, associez différents agents à différents personnages
- **Barre de Menus Uniquement** — Pas d'icône dans le Dock, fonctionne comme une app de barre d'état

## Installation

> [!TIP]
> **Dites à votre agent IA (OpenClaw / Claude Code) :**
>
> ```
> Installe oc-claw : curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **Ou exécutez-le vous-même dans le Terminal :**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> Vous pouvez aussi télécharger le DMG depuis les [Releases](https://github.com/rainnoon/oc-claw/releases). Après l'installation, exécutez `xattr -cr /Applications/oc-claw.app` pour contourner le Gatekeeper de macOS.

## Stack Technique

- **Tauri v2** + **React** + **TypeScript**
- Backend **Rust** pour l'interaction système et la communication API
- APIs natives macOS pour le positionnement de l'encoche et la gestion des fenêtres

## Développement

```bash
cd frontend
npm install
npx tauri dev
```

## Licence

MIT

---

<p align="center">
  <sub>Créé à l'origine lors du KAON Hackathon</sub>
</p>
