# tuxDrop — avancement

Plan : `~/.claude/plans/j-aimerais-cree-un-programme-purring-acorn.md`

## M1 — LocalSend v2 sur LAN, CLI autonome
- [x] Workspace cargo (`tuxdrop-core`, `tuxdrop`)
- [x] `proto.rs` : types LocalSend v2 + tests serde
- [x] `identity.rs` / `tls.rs` : cert auto-signé, fingerprint SHA-256, clé 0600, mTLS optionnel
- [x] `config.rs` / `trust.rs`
- [x] `discovery.rs` : multicast annonce/réponse (register HTTP, repli UDP)
- [x] `server.rs` + `receive.rs` : register / prepare-upload / upload / cancel / info
- [x] `client.rs` : envoi fichiers et dossiers, vérif. fingerprint, cancel
- [x] CLI : `info`, `peers`, `send`, `recv`, `trust`, `untrust`, `trusted`
- [x] Test d'intégration loopback (10 cas) + essai réel 2 instances sur ce PC
- [x] Interop manuelle avec l'app LocalSend officielle — validée par toi le 2026-10-22

## M2 — Démon et intégration bureau
- [x] `tuxdropd` : serveur + découverte permanente
- [x] API D-Bus `org.tuxdrop.Daemon1` (testée sur bus isolé)
- [x] Notifications Accepter / Refuser / Toujours accepter (code écrit, **à valider sur ton bureau**)
- [x] CLI : `status`, `accept`, `reject` via D-Bus
- [x] Unité `systemd --user` (`packaging/tuxdropd.service`)

## Paquets
- [x] `.deb` + `.rpm` via nfpm (`packaging/nfpm.yaml`), build glibc 2.36 dans Docker (`scripts/package.sh`)
- [x] Profils pare-feu ufw et firewalld livrés, non activés
- [x] Installation vérifiée dans `ubuntu:24.04` et `fedora:44`
- [ ] Signature des paquets / dépôt APT-DNF (si diffusion)

## Découverte fiable (Wi-Fi)
- [x] CLI `peers` / `send <alias>` via le démon quand il tourne (`Announce`, `ListPeers` a(ssssu))
- [x] Repli unicast : balayage /24 unique si personne ne répond en 1,5 s ; réponse UDP aussi en unicast
- [x] Logs debug découverte (annonce reçue, register reçu, réponse envoyée, balayage)
- [x] Validé PC1 (filaire) ↔ PC2 (Wi-Fi, power save) : peers 5/5 et 8/8 en autonome, envoi dans les deux sens
- [x] `tuxdrop trust` : empreinte complète ou appareil résolu (nom, alias, préfixe, IP), sinon erreur ; `untrust` par nom/alias/préfixe

## Noms locaux et confiance depuis l'icône
- [x] `names.json` (`NameStore`), nom jamais envoyé ; `Name (alias)` partout ; `send`/`trust`/`untrust` acceptent le nom
- [x] Icône : sous-menu par appareil (empreinte, « Rename… » via zenity, case « Trusted »), `· trusted` à côté du nom
- [x] Notifications : nom local seulement si l'expéditeur est vérifié
- [x] `ListPeers` → `a(sssssu)` (nom local), CLI et Nautilus à jour ; `tuxdrop name` ; zenity en Recommends
- [ ] Validation visuelle du sous-menu et de la boîte zenity sur ton bureau — **à faire par toi**

## Intégration bureau (icône + clic droit)
- [x] Icône de barre (StatusNotifierItem, `ksni`) dans `tuxdropd` : appareils proches, « Refresh »
- [x] Notifications pour les envois D-Bus `Send` (en cours / envoyé / échec)
- [x] Extension Nautilus `packaging/nautilus/tuxdrop.py` : sous-menu tuxDrop → appareils → envoi
- [x] Paquets : extension installée, Recommends nautilus-python + extension AppIndicator
- [x] Vérifié en conteneurs : menu Nautilus Ubuntu 24.04 → envoi reçu sur Fedora 44 ; menu de l'icône via dbusmenu
- [ ] Validation visuelle sur ton bureau (icône GNOME, clic droit dans Fichiers) — **à faire par toi**
- [ ] KDE/Dolphin : hors périmètre

## M3 — BLE + Wi-Fi Direct (après feu vert)
## M4 — UI GTK4 + Nautilus (après feu vert)
## Améliorations bureau (lot 2026-10-22 : 1, 2, 3, 4, 6, 8, 9)
- [x] 1. Progression dans la notification (envoi et réception, tous les 5 %, ≤ 1/s, hint `value`)
- [x] 2. Bouton « Cancel » (envoi : jeton d'annulation ; réception : `Receiver::cancel_local`) + D-Bus `Cancel(id)`
- [x] 3. « Open folder » sur « Files received » (`FileManager1.ShowItems`, éléments de premier niveau)
- [x] 4. Demande en attente dans l'icône (Accept / Decline / Always, icône en NeedsAttention)
- [x] 6. « Open received files » dans l'icône (`FileManager1.ShowFolders`)
- [x] 8. Paquets : `systemctl --global enable` au postinst, `disable` à la suppression ; opt-out par `mask`
- [x] 9. Messages texte LocalSend : notification « Message from … » + « Copy » (wl-copy / xclip), 204, limite 1 / 2 s
- [x] Corrigé au passage : progression côté envoi jamais traitée (motif `select!`), ordre ActionInvoked / NotificationClosed
- [x] Vérifié en conteneurs (faux serveur de notifications + FileManager1) Ubuntu 24.04 → Fedora 44 ; tests core 12/12
- [ ] Validation sur ton bureau : rendu GNOME des mises à jour de progression, Copy avec wl-copy — **à faire par toi**

## macOS (et Windows préparé) — 2026-10-25
- [x] Cœur portable : `sys.rs` (nom de l'ordinateur via `scutil`, interfaces physiques `en*`, renommage sans écrasement `RENAME_EXCL`, espace libre), `deviceModel` = OS réel
- [x] Correctif : l'expéditeur envoie `cancel` avant d'abandonner l'upload (le test `sender_cancel_cleans_partial_file` échouait sur macOS : le receveur voyait l'upload coupé avant l'annulation)
- [x] Démon découpé : logique commune (`main.rs`, `api.rs`, `tray.rs`) + `platform/freedesktop` (Linux, inchangé fonctionnellement) et `platform/native` (macOS, Windows)
- [x] macOS : icône de barre des menus (tray-icon), dialogues AppleScript (demande, message + Copy, renommer, choisir fichiers), bannières, Finder, pbcopy
- [x] CLI ↔ démon hors D-Bus : socket local JSON (`ipc.rs`), dans `DARWIN_USER_TEMP_DIR` (limite de 104 octets des chemins de socket)
- [x] Linux et macOS : menu « Send files… / Send folder… » par appareil, transferts en cours + Cancel dans le menu
- [x] Menu Partager du Finder : extension `tuxDropShare.appex` (Swift, `packaging/macos/share/`), fenêtre listant les appareils + Send ; sandboxée, elle lit le chemin du socket dans `control-socket` (écrit par le démon) ; icône de l'app générée (`make-icon.swift`)
- [x] `tuxdrop share` : le démon demande l'appareil (liste macOS / zenity) puis envoie ; méthode D-Bus `Share(as)` et `share` sur le socket (utile pour Dolphin/Thunar)
- [x] Image disque `.dmg` (app + lien Applications) par `scripts/package-macos.sh`, signature Developer ID + notarisation optionnelles (`CODESIGN_IDENTITY`, `NOTARY_PROFILE`, non testées faute de certificat) ; menu « Open at Login » (LaunchAgent) et enregistrement de l'extension Partager au lancement, pour une installation par glisser-déposer
- [x] `tuxDrop.app` + LaunchAgent (`scripts/package-macos.sh`, `scripts/install-macos.sh`)
- [x] Vérifié sur macOS : tests 34/34, démon + CLI isolés (status, envoi approuvé / non approuvé + accept CLI fermant la dialog, Send via socket), balayage sur en0
- [x] Vérifié Linux : clippy + tests + build release dans `rust:1.90-bookworm` ; Windows : `cargo check`/clippy en cible croisée
- [ ] À faire par toi : `scripts/install-macos.sh`, autorisations Réseau local / Téléchargements, échange réel avec un téléphone ou le PC Linux
- [ ] Windows : notifications toast, dialogues, presse-papiers, démarrage auto, menu Explorer (liste dans `platform/native/windows.rs`)

## M5 — AirDrop natif via OWL (exploratoire)
- [x] État des lieux et plan : `docs/airdrop.md` (branche `airdrop`), **en attente de validation**

## Hors périmètre pour l'instant
- API download (`prepare-download` / `download`, envoi vers un navigateur)
