# Compatibilité AirDrop — notes et plan (en attente de validation)

Branche : `airdrop`. Rien n'est implémenté pour l'instant ; ce document rassemble l'état des lieux
du 2026-10-22 et le plan proposé.

## 1. Comment fonctionne AirDrop

Quatre couches, de bas en haut :

1. **BLE (réveil)** : l'émetteur diffuse une annonce Bluetooth LE Apple pendant qu'il cherche des
   destinataires. Les appareils en mode « Tout le monde » s'y réveillent et allument AWDL.
   - Données fabricant Apple (`0x004C`), TLV de type `0x05`, 18 octets, version 3.
   - Sans identité Apple, les 4 empreintes de 2 octets sont à zéro : seuls les appareils en mode
     « Tout le monde » se réveillent.
   - Recommandé : annonce connectable, intervalle 100 ms.
2. **AWDL** (Apple Wireless Direct Link) : lien Wi-Fi direct propriétaire d'Apple, en IPv6
   link-local, sur les canaux 6, 44 ou 149.
3. **mDNS / DNS-SD** : service `_airdrop._tcp.local.` annoncé sur l'interface AWDL.
   - Nom d'instance : 12 chiffres hexadécimaux aléatoires, réutilisé comme `SenderID`.
   - TXT : `flags=` ; un récepteur minimal annonce `0x089`.
4. **HTTPS** (HTTP/1.1, corps en plist binaire), port 8770 par convention (Owly reçoit sur 8771).
   - **TLS** : chiffrement seul. Certificats auto-signés acceptés, pas de vérification de nom, pas de
     certificat client côté émetteur.
   - **`POST /Discover`** : le récepteur répond `StatusCode` 100 (mode « Tout le monde »), 200
     (contact reconnu) ou 401 (« Contacts uniquement » sans correspondance). L'émetteur ne liste le
     récepteur que si la réponse contient `ReceiverComputerName`.
   - **`POST /Ask`** : la liste des fichiers (`FileName`, type UTI, `FileSize`…) ou des liens.
     - 200 vaut acceptation ; toute autre réponse, une fermeture ou l'expiration vaut refus.
     - Le récepteur garde la demande au maximum 15 s.
   - **`POST /Upload`** : archive cpio (format odc), compressée en DVZip (`application/x-dvzip`) ou
     en gzip (`application/x-cpio`).

Modes de visibilité côté iPhone :
- **« Contacts uniquement »** : exige un enregistrement d'identité signé par Apple (CMS signé,
  chaîne jusqu'à la racine Apple). Impossible à produire pour nous.
- **« Tout le monde pendant 10 minutes »** : seul mode accessible. Depuis iOS 16.2, il repasse en
  « Contacts uniquement » au bout de 10 minutes. C'est aussi le mode exigé par les Pixel.

## 2. Ce qui existe déjà (installé sur PC1)

**Owly** ([t4t5/owly](https://github.com/t4t5/owly), licence MIT), paquets `owly` 0.4.0 et
`omdrop-owl` 0.3.0.

`owlyd` est un service système (root, `CAP_NET_ADMIN`/`CAP_NET_RAW`) qui fait tourner AWDL en
espace utilisateur et prête la radio via D-Bus.

API `io.github.t4t5.Owly.Radio1` :

| Membre | Rôle |
|---|---|
| `Probe() → s` | Rapport de capacités en JSON (mode, canal, problèmes connus). Ouvert à tous. |
| `Acquire(u durée_s) → (t bail, a{sv} lien)` | Lève la radio et la prête. `0` = jusqu'à libération. Contrôlé par polkit `io.github.t4t5.Owly.acquire` (`allow_active=yes`). |
| `Release(t bail)` | Rend la radio ; retourne une fois le Wi-Fi revenu. |
| `AcquireDetached` | Réservé à root ; le bail survit à l'appelant. |
| Propriétés | `State` (`idle`/`starting`/`up`/`stopping`), `Lease`, `Link`, `Peers`, `LastEnd`. |
| Signal `StateChanged(t, s, s)` | Bail, nouvel état, raison. |

- **Lien (`Link`)** : `interface` (awdl0), `index`, `mac`, `address` (fe80::…), et
  `announce` (uuu) en option, le rythme des rafales mDNS.
- **Fin d'un bail** : sur `Release`, à la fin de la durée, si le client quitte le bus, ou si le
  lien est perdu.

**Crate `owly-airdrop` 0.1.0** (crates.io, MIT ; API 0.x susceptible de changer) :
- **`proto`** : le cœur sans IO (plist, HTTP, cpio/gzip/DVZip et règles d'extraction, messages,
  annonce BLE `ble::Advert::anonymous(tag).manufacturer_data()`, enregistrements mDNS).
- **`driver`** (tokio + rustls) :
  - `receiver::Receiver::bind(addr, identity, Options)` → événements `Discovered`,
    `Request(accept/decline)`, `Withdrawn`, `Urls`, `Received{paths}`, `Failed` ;
  - `sender::Sender::{discover, send}` avec `Event::{Accepted, Progress}` ;
  - `staging` : extraction dans un dossier isolé, renommage en cas de collision ;
  - `tls::Identity::load_or_generate(dir)`.
- **Feature `mdns`** : `Mdns::new(index_interface).with_burst(..)`, avec `register(Service)` et
  `browse()`.
- **À l'intégration** : l'application doit installer le fournisseur crypto rustls
  (`rustls::crypto::ring::default_provider().install_default()`).
- **Non fourni** : l'émission BLE. La CLI `owly` passe par BlueZ (`LEAdvertisingManager1`), mais ce
  code est dans `owly-cli`, qui n'est pas publié.

**CLI `owly`** : `doctor`, `radio up`, `receive`, `send`, `browse`, `test-card`.

Autres projets :
- [opendrop-rs](https://github.com/minios-linux/opendrop-rs) : portage Rust d'OWL et d'OpenDrop,
  licence GPLv3. Validé en réception depuis iOS 18. Il n'émet pas d'annonce BLE, donc ne peut pas
  envoyer vers un iPhone en veille.
- [OWL](https://github.com/seemoo-lab/owl) et OpenDrop : implémentations historiques (C et Python).

**État de PC1** (`owly doctor`) :
- carte Intel BE200 (`8086:272b`, iwlwifi), statut « tested », prête ;
- mode *exclusive*, canal 6 (44 interdit en émission, 149 possible mais pas de cellule ad-hoc en
  5 GHz sur Intel) ;
- lent : une photo de 2,4 Mo prend 10 à 17 s ;
- Bluetooth : rôle périphérique disponible, 12 annonces simultanées possibles.

## 3. Pourquoi le Wi-Fi est coupé (alors que ce n'est pas le cas chez Apple)

**Chez Apple, AWDL est dans le firmware de la puce Wi-Fi (Broadcom).** La puce partage son temps
entre le canal de la box et le canal AWDL, par créneaux de quelques millisecondes, synchronisés avec
les autres appareils. Pendant qu'elle est sur AWDL, elle signale à la box qu'elle « dort », et la box
garde ses paquets.

**Sous Linux, la carte (ici Intel) ne connaît pas AWDL.** Owly le simule en injectant des trames
brutes via une interface monitor. Deux contraintes s'ajoutent :

1. **Accusés de réception** : chaque trame reçue doit être acquittée en une dizaine de
   microsecondes, ce que seul le firmware fait, et uniquement pour une interface qu'il gère
   (station, ad-hoc, point d'accès) sur son canal. Owly crée donc une cellule ad-hoc
   (`owlyibss0`) sur l'identifiant AWDL, pour que le firmware acquitte les trames de l'iPhone.
2. **Une carte, un canal, un rôle** : le firmware Intel n'injecte pas pendant qu'il est connecté à
   une box, et ne tient pas la connexion à la box en même temps que la cellule ad-hoc. D'où le mode
   *exclusive* : le Wi-Fi est coupé pendant la session. Intel refuse aussi la cellule en 5 GHz, d'où
   le canal 6.

Changer de canal depuis Linux prend des millisecondes et fait décrocher la box : impossible
d'imiter le va-et-vient d'Apple depuis l'espace utilisateur.

**Pour éviter la coupure :**
- **Carte compatible avec le mode *shared*** (MT7925, RTL8852AE/BE/CE) : elle injecte tout en
  restant connectée, et la connexion à la box acquitte les trames de l'iPhone. La box doit être sur
  le canal 6, 44 ou 149.
- **Deuxième carte** (clé USB, par exemple MT7612U) dédiée à AirDrop :
  `station = "<clé>"` dans `/etc/owly/radio.toml`.
- **Machine qui n'utilise pas son Wi-Fi** (PC1 est en Ethernet) : la coupure ne gêne rien.
- **Plus tard, Wi-Fi Aware (NAN)**, l'équivalent standard d'AWDL géré par le firmware. La BE200
  annonce la combinaison « managed ≤ 2 + NAN ≤ 1, 2 canaux », donc en même temps qu'une connexion
  normale. Ce n'est utile que si Apple fait passer AirDrop dessus.

## 4. Limites et risques

- **iPhone en « Tout le monde pendant 10 minutes » uniquement** ; « Contacts uniquement » est
  inaccessible.
- **15 s pour accepter** une demande reçue (délai imposé par le protocole).
- **Liens reçus d'un émetteur non authentifié** : partiellement pris en charge (voir
  [opendrop-rs](https://github.com/minios-linux/opendrop-rs)).
- **Gros envois** : ils peuvent caler en fin de transfert avec une seule radio monitor
  (half-duplex) ; c'est signalé par opendrop-rs.
- **Avenir d'AWDL** : l'UE a demandé à Apple d'abandonner AWDL au profit de Wi-Fi Aware
  (Wikipedia, qui cite Ars Technica du 2025-11-20). L'application effective n'est pas vérifiée ;
  AWDL fonctionne encore avec iOS 26 d'après Owly.
- **Dépendances 0.x** : `owly-airdrop` peut changer d'API, et `owlyd` est un service tiers à
  installer (publié sur GitHub, pas dans les dépôts des distributions).

## 5. Plan proposé (à valider)

Architecture :
- **`owlyd`** fournit AWDL. tuxDrop ne gère jamais la carte Wi-Fi lui-même et ne tourne pas en root.
- **`owly-airdrop`** fournit le protocole.
- **tuxDrop** apporte l'intégration (même démon, notifications, CLI, futur GUI), le réveil BLE et la
  politique d'utilisation de la radio.

1. **Dépendance** : `owly-airdrop` 0.1 (feature `mdns`) dans `tuxdropd` ; installer le fournisseur
   crypto rustls au démarrage.
2. **`tuxdropd/src/airdrop/radio.rs`** : client D-Bus d'`owlyd` (`Probe`, `Acquire(durée)`,
   `Release`, `StateChanged`, lecture du lien).
3. **`airdrop/ble.rs`** : annonce BLE de réveil via BlueZ en zbus (objet `LEAdvertisement1`
   exporté, données fabricant Apple). Active seulement pendant une recherche ou un envoi.
4. **`airdrop/mod.rs`** : la session AirDrop tant que la radio est levée :
   - récepteur en mode « Tout le monde » sur `[fe80::…%awdl0]:8771` ;
   - annonce et recherche mDNS ;
   - identité TLS dans `~/.local/share/tuxdrop/airdrop/`.
5. **Réception, même circuit que LocalSend** :
   - notification Accepter / Refuser, `tuxdrop status`/`accept`/`reject`, signaux D-Bus ;
   - pas de « Toujours accepter » (aucune identité vérifiable) ;
   - liens reçus : notification seulement, jamais ouverts automatiquement ;
   - il faut généraliser la structure `Pending` du démon (réponse LocalSend ou AirDrop).
6. **Visibilité à la demande** :
   - `tuxdrop airdrop on [--minutes 10]` et `tuxdrop airdrop off` : radio acquise pour la durée,
     puis le Wi-Fi est rendu ;
   - option `airdrop_always_on` dans `config.toml` pour les machines dont le Wi-Fi ne sert pas
     (PC1) ;
   - méthodes D-Bus correspondantes.
7. **Envoi** : `tuxdrop airdrop browse` et `tuxdrop airdrop send <nom> <fichiers…>`, exécutés par
   le démon : radio, réveil BLE, mDNS, `/Discover`, `/Ask`, `/Upload`, puis radio rendue. La
   progression passe par les signaux existants. C'est séparé de `tuxdrop send` (LocalSend), car la
   mise en route prend 10 à 30 s et coupe le Wi-Fi sur Intel.
8. **`tuxdrop airdrop doctor`** : `Probe` d'`owlyd`, état de BlueZ, règles de pare-feu attendues.
9. **Paquets** : `Recommends: owly` (deb et rpm). Pare-feu documenté (TCP 8771 et UDP 5353 sur
   `awdl0`), sans ouverture automatique.
10. **Tests** :
    - intégration en local entre l'émetteur `owly-airdrop` et notre récepteur (demande →
      acceptation → fichier) ;
    - tests unitaires (lien, annonce BLE) ;
    - fmt, clippy, tests, paquets.
11. **Validation réelle sur PC1 avec un iPhone** : d'abord vérifier qu'un service utilisateur
    peut acquérir la radio (polkit), puis iPhone → PC1 et PC1 → iPhone.
12. **Doc** : section AirDrop dans `docs/commands.md`, README, `PROGRESS.md`.

## 6. À vérifier en premier

- **Polkit et service utilisateur** : un processus d'un service `systemd --user` n'appartient pas à
  une session de connexion. Polkit doit le rattacher à la session graphique de l'utilisateur ;
  sinon `Acquire` demandera une authentification administrateur. Test : un `Acquire` de quelques
  secondes via `systemd-run --user`.
- **Pare-feu de PC1 (ufw actif)** : autoriser TCP 8771 et UDP 5353 en entrée sur `awdl0`, par
  exemple `sudo ufw allow in on awdl0 to any port 8771 proto tcp` et
  `sudo ufw allow in on awdl0 to any port 5353 proto udp`.
- **Un seul bail radio à la fois** : si `owly radio up` (ou omdrop) tient déjà la radio, tuxDrop
  reçoit `Busy` et doit l'expliquer.
- **PC2** (Fedora, Wi-Fi seulement) : le mode exclusif lui couperait le Wi-Fi, y compris la session
  SSH. Ne pas l'utiliser pour les tests AirDrop.

## Sources

- [Owly (t4t5)](https://github.com/t4t5/owly) — README, `docs/SPEC.md`, API `owlyd`
- [owly-airdrop (crates.io)](https://crates.io/crates/owly-airdrop), [omdrop-owl](https://github.com/t4t5/omdrop-owl)
- [opendrop-rs](https://github.com/minios-linux/opendrop-rs), [OWL (seemoo-lab)](https://github.com/seemoo-lab/owl), [fork OWL MT7921](https://github.com/jedbillyb/owl)
- [AirDrop (Wikipedia)](https://en.wikipedia.org/wiki/AirDrop)
- [Quick Share ↔ AirDrop sur Pixel 10 (9to5Google)](https://9to5google.com/2025/11/20/android-quick-share-airdrop-pixel-10/)
- [DMA — interopérabilité Apple](https://digital-markets-act.ec.europa.eu/developer-portal/interoperability/apple-specification-proceedings-interoperability-connected-devices-and-request-based-process_en)
