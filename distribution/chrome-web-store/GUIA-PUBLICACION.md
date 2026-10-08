# Guía: publicar la extensión Synchrome en la Chrome Web Store

Estado al 8 de octubre de 2026. Cada paso dice **quién** lo hace: **tú** (en el panel de la Web
Store, con tu cuenta) o **yo** (en el repo / la app).

## Lo que ya está hecho

| Elemento | Dónde | Estado |
|---|---|---|
| Paquete para subir (sin campo `key`, 9 ficheros, 5 permisos) | `dist/chrome-web-store/Synchrome-1.0.2-ChromeWebStore.zip` (regenerar: `python3 scripts/package-extension.py`) | listo, SHA-256 `a4cf5bb5…` (determinista) |
| Icono de tienda 128×128, promo pequeña 440×280, 2 capturas 1280×800 | `distribution/chrome-web-store/assets/` | listos (cumplen los tamaños oficiales) |
| Textos del listing (nombre, resumen, descripción) | `distribution/chrome-web-store/listing.md` | listos |
| Pestaña Privacy (propósito único, justificación de permisos, datos) | `distribution/chrome-web-store/privacy-disclosures.md` | listos |
| Política de privacidad pública | https://www.garduc.com/synchrome/privacy/ | publicada (HTTP 200 comprobado hoy) |
| Instrucciones para el revisor | `distribution/chrome-web-store/reviewer-instructions.md` | listas, faltan 2 datos (ver fase 4) |

## El punto crítico: el ID de la extensión

La app nativa solo acepta conexiones de la extensión con ID `gmpmcnknoaogpjdoonlddhpdpgkebbog`
(el que usamos en desarrollo). **No existe la clave privada de ese ID** (no se guardó en el
slice 6), así que la Web Store **asignará un ID nuevo** al subir el paquete. Hasta que la app
acepte ese ID nuevo, la extensión instalada desde la tienda **no podrá conectarse**. Por eso el
orden es: crear el borrador → obtener el ID oficial → yo adapto la app → nueva versión de la app
→ enviar a revisión.

---

## Fase 1 — Revisar la cuenta de desarrollador (tú, ~10 min, una sola vez)

En https://chrome.google.com/webstore/devconsole → menú izquierdo **Account**:

1. **Publisher name**: el nombre público que verán los usuarios (p. ej. «Garduc» o tu nombre).
2. **Contact email**: debe estar **verificado** (si aparece «Verify», pulsa y confirma el correo).
   Es el email que verán los usuarios; el de soporte que usamos es `xavirn.me@gmail.com`.
3. **Trader / Non-trader** (obligatorio en la UE): declara lo que corresponda. Si eliges
   *Trader*, la dirección y el teléfono (verificado por SMS) **se publican** en la ficha.
   Para una extensión gratuita personal lo habitual es *Non-trader*, pero es una decisión
   legal tuya.
4. **Notifications**: activa los avisos por email de revisión y publicación.
5. Comprueba que la cuenta de Google tiene la **verificación en dos pasos** activada (la tienda
   la exige para publicar).

## Fase 2 — Crear el borrador y obtener el ID oficial (tú, ~5 min)

1. En el panel: **Add new item** → **Choose file** →
   `dist/chrome-web-store/Synchrome-1.0.2-ChromeWebStore.zip` → **Upload**.
2. **No envíes nada a revisión todavía.** Solo queremos el borrador.
3. Pestaña **Package** → copia el **Item ID** (32 letras a–p) y pulsa **View public key** →
   copia el bloque completo (`-----BEGIN PUBLIC KEY-----` … `-----END PUBLIC KEY-----`).
4. **Pásamelos** (los dos son datos públicos, no son secretos).

## Fase 3 — Adaptar la app al ID oficial (yo)

Con el Item ID y la clave pública:

1. Pongo la clave pública oficial en el `key` del manifest de desarrollo → la extensión cargada
   en local tendrá **el mismo ID** que la de la tienda (un solo ID en todas partes).
2. La app acepta el ID oficial (native messaging `allowed_origins`, escritura del token de
   conexión en el almacenamiento de esa extensión) y **sigue aceptando** el ID antiguo durante la
   transición, para no romper conexiones existentes.
3. Tests + E2E real con Chrome desechable usando el ID oficial.
4. Nueva versión firmada de la app (1.0.x) con su DMG.
5. Te dejo el `status.json` y las instrucciones del revisor actualizadas.

## Fase 4 — Lo que necesita el revisor de Google (tú)

El revisor tiene que poder instalar la app de macOS y probar la extensión:

1. **Notarizar la app (muy recomendable antes de enviar a revisión).** Sin notarizar, macOS
   bloquea la app en el Mac del revisor y probablemente la rechacen. Necesitas una API key de
   App Store Connect:
   1. https://appstoreconnect.apple.com → **Users and Access** → **Integrations** →
      **App Store Connect API** → **Team Keys** → **+** → nombre «Synchrome notary», acceso
      **Developer** → **Generate**.
   2. Descarga el fichero `AuthKey_XXXXXX.p8` (solo se puede descargar una vez) y apunta el
      **Key ID** y el **Issuer ID**.
   3. Guárdalo fuera del repo, por ejemplo en `~/.appstoreconnect/private_keys/`.
   4. Para la build de release, exporta en la terminal (no en ningún fichero del repo):
      ```bash
      export APPLE_API_KEY="<Key ID>"
      ```
      ```bash
      export APPLE_API_ISSUER="<Issuer ID>"
      ```
      ```bash
      export APPLE_API_KEY_PATH="$HOME/.appstoreconnect/private_keys/AuthKey_<Key ID>.p8"
      ```
      y avísame: `scripts/release-macos.sh` notariza y hace *staple* automáticamente con esas
      variables. (Alternativa: me dices dónde está el `.p8` y los dos IDs y lo lanzo yo.)
2. **URL pública de descarga del DMG** (la del Escritorio no sirve para Google): por ejemplo
   `https://www.garduc.com/synchrome/download/Synchrome_1.0.x_aarch64.dmg` o una GitHub Release
   pública. Yo genero el DMG; tú decides dónde publicarlo.
3. **Cuenta de prueba para el revisor**: el servidor permite registrarse desde la app (sin
   confirmar email), así que el revisor puede crear la suya. Si prefieres darle una, crea una
   cuenta **nueva y dedicada** (no las de los E2E, que se van a borrar) y pon sus credenciales
   solo en el campo privado del panel (fase 5, *Test instructions*), nunca en el repo.

## Fase 5 — Rellenar el borrador (tú, ~20 min)

Abre el borrador en el panel. Copia y pega desde los ficheros indicados.

**Package**: si yo subí una versión nueva del paquete en la fase 3, súbela con
**Upload new package** (el Item ID no cambia).

**Store listing** (desde `listing.md`):
- *Description*: el bloque «Detailed description».
- *Category*: Productivity (subcategoría más cercana: *Workflow & Planning* o *Tools*).
- *Language*: English.
- *Store icon*: `assets/store-icon.png`.
- *Screenshots*: `assets/screenshot-workspace.png` y `assets/screenshot-companion.png`.
- *Small promo tile*: `assets/promo-small.png`.
- *Official URL / Homepage*: déjalo vacío hasta que exista una página de Synchrome
  (o pon la de descarga de la fase 4).
- *Support URL*: opcional (`mailto:` no se admite; si no hay web de soporte, déjalo vacío; el
  email de contacto de la cuenta ya sirve).

**Privacy** (desde `privacy-disclosures.md`):
- *Single purpose*: el párrafo «Single purpose».
- *Permission justification*: una justificación por permiso (`bookmarks`, `management`,
  `nativeMessaging`, `storage`, `alarms`).
- *Remote code*: **No, I am not using remote code**.
- *Data usage*: marca **Web history**, **Website content**, **Authentication information** y
  **Personally identifiable information** (con el criterio conservador explicado en el
  fichero); deja sin marcar el resto.
- Marca las tres **certificaciones** (no se venden datos, no se usan para fines ajenos, no para
  solvencia crediticia).
- *Privacy policy URL*: `https://www.garduc.com/synchrome/privacy/`.

**Distribution**:
- *Payments*: Free.
- *Visibility*: recomiendo **Unlisted** para el primer lanzamiento (solo instala quien tiene el
  enlace, que pondremos en la app) y pasar a **Public** cuando lo hayas probado. *Private*
  (solo testers de confianza) también vale para una prueba previa.
- *Regions*: todas.

**Test instructions**: pega `reviewer-instructions.md` completando la URL de descarga del DMG
(fase 4) y, si das una cuenta de prueba, sus credenciales.

## Fase 6 — Enviar a revisión (tú)

1. Comprueba que no quedan avisos en rojo en ninguna pestaña.
2. **Submit for review** → en el diálogo **desmarca** «publicar automáticamente tras la
   aprobación» (publicación diferida). Así publicas tú cuando quieras.
3. La revisión suele tardar de unos días a un par de semanas (más si piden aclaraciones sobre
   `management` o `nativeMessaging`). Si encuentras un error, **Cancel review**, corriges y
   reenvías.
4. Tras la aprobación tienes **30 días** para publicar; si no, vuelve a borrador.

## Fase 7 — Después de la aprobación

1. **Tú**: **Publish**.
2. **Yo**: instalo la extensión real desde la tienda en un perfil desechable de Chrome y hago el
   E2E completo con el ID oficial; luego cambio la guía de la app («Load unpacked») por el enlace
   de la tienda y saco la versión correspondiente de la app.
3. Actualizamos `status.json` (`storeItemId`, `reviewSubmitted`, `published`).

## Qué me tienes que pasar, en resumen

1. Item ID + public key del borrador (fase 2).
2. Los datos de notarización o el aviso de que has exportado las variables (fase 4.1).
3. Dónde se publicará el DMG (fase 4.2).

Referencias oficiales:
[publicar](https://developer.chrome.com/docs/webstore/publish) ·
[imágenes](https://developer.chrome.com/docs/webstore/images) ·
[privacidad](https://developer.chrome.com/docs/webstore/cws-dashboard-privacy) ·
[distribución](https://developer.chrome.com/docs/webstore/cws-dashboard-distribution) ·
[clave e ID](https://developer.chrome.com/docs/extensions/reference/manifest/key) ·
[cuenta y trader](https://developer.chrome.com/docs/webstore/set-up-account)
