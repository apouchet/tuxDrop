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
- [ ] Interop manuelle avec l'app LocalSend officielle (iPhone / Android / Flatpak) — **à faire par toi**

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

## M3 — BLE + Wi-Fi Direct (après feu vert)
## M4 — UI GTK4 + Nautilus (après feu vert)
## M5 — AirDrop natif via OWL (exploratoire)

## Hors périmètre pour l'instant
- API download (`prepare-download` / `download`, envoi vers un navigateur)
- Annulation d'un envoi lancé via D-Bus `Send`
