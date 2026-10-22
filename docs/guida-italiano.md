# FerroTherm — Guida in italiano

Autore: **Roany Romano** · Licenza MIT / Apache-2.0

Questa guida spiega come installare FerroTherm e come fare una simulazione
completa, dall'import dei dati CAD alla lettura dei risultati. Per il
riferimento completo di tutte le chiavi dei file caso vedi
[`case-format.md`](case-format.md) (in inglese).

---

## 1. Cos'è FerroTherm

FerroTherm è un solutore termico e fluidodinamico 3D per apparati
elettronici: calcola le temperature di schede, componenti e contenitori,
con o senza flusso d'aria. Il metodo è quello dei volumi finiti su griglia
cartesiana, lo stesso tipo usato dai programmi commerciali di thermal
management elettronico, ma FerroTherm è un progetto indipendente e **non è
certificato**: confronta sempre i risultati con misure o con un altro
strumento prima di usarli per una qualifica.

Cosa sa fare:

- conduzione 3D stazionaria e transitoria, materiali ortotropi, k(T);
- flusso d'aria (Navier–Stokes, galleggiamento di Boussinesq, turbolenza
  LVEL) accoppiato alla conduzione nei solidi;
- ventole con curva P–Q, aperture, ingressi a portata imposta, griglie e
  piastre forate, dissipatori alettati;
- convezione e irraggiamento verso l'esterno, irraggiamento interno tra
  superfici, carico solare;
- componenti con modello compatto 2R (θJC/θJB) o DELPHI multi-nodo e
  verifica PASS/FAIL della Tj (derating NSWC Crane SD-18 o limite utente);
- import di STEP, STL, IDF, KiCad, Gerber + Excellon, IPC-2581, ODB++;
- export di STEP, STL, IDF, VTK (ParaView) e CSV.

---

## 2. Installazione

### Con i programmi già compilati (consigliato)

Dalla pagina **Releases** del repository GitHub scarica l'archivio per il
tuo sistema:

- **Windows:** `ferrotherm-…-x86_64-pc-windows-msvc.zip` → estrai e avvia
  `ferrotherm-gui.exe`.
- **macOS (Apple Silicon M1/M2/M3…):** `…-aarch64-apple-darwin.tar.gz`;
  **macOS Intel:** `…-x86_64-apple-darwin.tar.gz` → estrai e apri
  `FerroTherm.app`. La prima volta: tasto destro → *Apri* (l'app non è
  notarizzata da Apple).
- **Linux:** `…-x86_64-unknown-linux-gnu.tar.gz` → `ferrotherm-gui`.

### Compilando dai sorgenti

1. Prerequisiti:
   - Windows: *Build Tools di Visual Studio* con "Sviluppo di applicazioni
     desktop con C++", poi Rust da <https://rustup.rs>;
   - macOS: `xcode-select --install`, poi Rust da <https://rustup.rs>.
2. Serve Rust 1.95 o successivo (`rustup update`).
3. Nella cartella del progetto:

   ```bash
   cargo build --release
   ```

   (10–15 minuti la prima volta). I programmi sono in `target/release/`:
   `ferrotherm-gui` (interfaccia grafica) e `ferrotherm` (riga di comando).

---

## 3. L'interfaccia in breve

```
┌────────────┬──────────────────────────────┬───────────────┐
│ Albero del │        Vista 3D              │  Proprietà    │
│ modello    │  (geometria / risultati)     │  dell'oggetto │
│            │                              │  selezionato  │
├────────────┴──────────────────────────────┴───────────────┤
│ Results · Convergence · Messages                          │
└───────────────────────────────────────────────────────────┘
```

- **Albero (sinistra):** progetto, dominio, griglia, ambiente, solutore,
  *Air flow*, *Radiation*, poi blocchi, componenti, condizioni al contorno,
  ventole, griglie, dissipatori, punti di monitoraggio. Il `+` aggiunge un
  oggetto; la casella accanto a un blocco lo nasconde nella vista.
- **Vista 3D (centro):** trascina per ruotare, tasto destro (o Shift) per
  spostare, rotella per lo zoom, doppio clic per inquadrare tutto, clic per
  selezionare. *X-ray* mostra l'interno, *Section* taglia il modello lungo
  x, y o z, *Boundaries* mostra dove sono applicate le condizioni al
  contorno.
- **Proprietà (destra):** tutti i parametri dell'oggetto selezionato.
  Passando il mouse su materiali, package e limiti vedi valori e fonte.
- **Pannello in basso:** *Results* (tabelle Tj con PASS/FAIL, blocchi,
  calore uscente da ogni superficie), *Convergence* (residui durante il
  calcolo, ventole, aperture, punti di monitoraggio), *Messages*.

Scorciatoie: **F5** risolve, Ctrl/Cmd+Z annulla, Ctrl/Cmd+Y ripete,
Ctrl/Cmd+D duplica, Canc elimina, Ctrl/Cmd+S salva.

**Unità:** mm, °C, W, W/(m²·K), s, m³/s, Pa.

---

## 4. Tutorial A — scheda da KiCad/ODB++/IPC-2581 in un contenitore sigillato

Obiettivo: temperatura di giunzione dei componenti di una scheda montata in
una scatola di alluminio chiusa, raffreddata per convezione naturale.

### 4.1 Preparare la lista delle potenze

Un file CSV con una riga per componente (separatore virgola o punto e
virgola; l'intestazione è facoltativa):

```csv
refdes,power_W,Rjc,Rjb,Tj_max
U1,2.5,0.4,6.0,
U5,0.8,,,
Q1,1.2,1.5,,125
```

Colonne: riferimento, potenza [W] e, facoltativi, θJC [K/W], θJB [K/W],
Tj massima [°C]. I componenti con θJC o θJB diventano **modelli compatti 2R**;
gli altri diventano blocchi di resina con la potenza distribuita nel volume.
**I valori θ devono venire dal datasheet**: FerroTherm non li inventa.

### 4.2 Importare la scheda

1. *File → Import CAD…* e scegli il file: `.kicad_pcb`, la cartella o lo
   zip dei Gerber (con il file di foratura), il `.tgz` ODB++, l'`.xml`
   IPC-2581 o l'`.emn` IDF. In alternativa trascina il file sulla finestra.
2. La finestra di import mostra dimensioni, spessore, rame per strato e
   un'anteprima dall'alto (grigio = lato top, blu = lato bottom,
   arancione = componenti con potenza).
3. Imposta:
   - **Board corner at:** dove mettere l'angolo della scheda nel modello
     (es. `10, 10, 10` mm se la scheda sta su distanziali di 10 mm);
   - **PCB map tile:** 10 mm va bene di solito. La scheda viene divisa in
     tessere, ognuna con la percentuale di rame di ogni strato e di via
     misurata dal disegno: un piano di massa pieno conduce molto più di una
     zona di sole piste;
   - **Components:** *2R compact* (consigliato);
   - **Load CSV…:** carica la lista potenze del punto 4.1;
   - **Height when unknown:** se il CAD non contiene le altezze, FerroTherm
     le stima dal nome del package e lo segnala nei messaggi: controllale.
4. *Add to model*.

### 4.3 Aggiungere il contenitore

Con il `+` di **BLOCKS** aggiungi le pareti come blocchi (es. fondo, quattro
pareti, coperchio in `al-6061-t6`) e i distanziali. I blocchi aggiunti dopo
sovrascrivono quelli precedenti dove si sovrappongono.

In alternativa importa la meccanica da STEP (*File → Import CAD…*, scelta
*Voxelised shape*, vedi tutorial C).

### 4.4 Ambiente e condizioni al contorno

- **Environment:** scegli un preset MIL-STD-810H (es. *A1 hot-dry*, 49 °C
  operativo) oppure nessun preset e la temperatura su ogni contorno.
- **BOUNDARIES (+):** per ogni faccia esterna:
  - *Type: Convection*, `h` = 3–10 W/(m²·K) per convezione naturale,
    20–100 per aria forzata esterna;
  - *Emissivity → finish*: es. alluminio anodizzato nero (ε ≈ 0,86).
  Le facce senza condizione sono **adiabatiche**.

### 4.5 Griglia e calcolo

- **Grid → Max cell size:** inizia con 3–4 mm; le schede hanno comunque
  almeno 4 celle nello spessore.
- **F5**. Un modello di sola conduzione si risolve in pochi secondi.
- Poi ripeti con celle più piccole (es. 2 mm): se le Tj cambiano di meno
  di qualche decimo di grado, la griglia è sufficiente.

### 4.6 Leggere i risultati

- La vista passa a **Temperature**. Passa il mouse su un punto per leggere
  la temperatura locale; usa *Section* per vedere l'interno.
- In *Results*: per ogni componente Tj, Tcase, Tboard, ripartizione del
  calore case/board, limite, margine e **PASS/FAIL**.
- Controlla lo **sbilancio energetico** (*imbalance*) nella riga riassuntiva:
  deve essere vicino a zero.
- *Export results…* scrive i file `.vtr` per ParaView, `report.md`,
  `report.json`, `components.csv`, `blocks.csv`.

---

## 5. Tutorial B — apparato ventilato (CFD)

Apri l'esempio `examples/forced_air_chassis.toml` (*File → Open*), che
contiene già tutto, oppure parti dal tuo modello.

### 5.1 Attivare il flusso d'aria

1. Seleziona **Air flow** nell'albero e spunta *Solve the air flow*.
2. Il materiale di riempimento del dominio deve essere un fluido con
   viscosità: se è `air-still` il programma propone il pulsante
   **Use 'air'**.
3. Turbolenza **LVEL** (adatta all'elettronica), schema *Power law*.

### 5.2 Ventole

**FANS (+)**:
- *Centre* = centro della faccia della ventola, *Blows towards* = verso del
  flusso (`+x`, `-z`, …);
- *Round* con diametro e mozzo, oppure *Rectangular*;
- *Fan curve*: inserisci i punti della curva del datasheet (portata in
  m³/s o in CFM, pressione statica in Pa) con portata crescente. Il
  solutore trova da solo il punto di lavoro sulla resistenza del sistema.
  In alternativa *Fixed flow*.

Una ventola sulla parete del dominio aspira o soffia verso l'ambiente; una
ventola all'interno è un salto di pressione.

### 5.3 Aperture e griglie

- **Aperture:** una condizione al contorno di tipo *Opening / vent*,
  eventualmente limitata a un rettangolo (*Region*). L'aria entra o esce
  alla pressione ambiente, alla temperatura indicata in *Air temperature in*.
  Spunta *Grille / perforated plate on the vent* per aggiungere la perdita
  di carico.
- **Griglie interne, filtri, piastre forate:** oggetto **GRILLES (+)**.
- **Coefficiente di perdita ζ:** Δp = ½·ρ·ζ·u² sulla velocità di
  avvicinamento. Il valore viene dal diagramma di Idelchik, dal datasheet
  della griglia o da una prova. Se inserisci anche frazione libera e
  diametro dei fori, viene aggiunto il termine laminare 30/(f²·Re).

### 5.4 Dissipatori e punti di monitoraggio

- **HEAT SINKS (+):** base (lunghezza lungo le alette × larghezza ×
  spessore), direzione delle alette, numero, spessore e altezza delle
  alette. Il pannello mostra il passo tra le alette: servono almeno 2–3
  celle in ogni canale, quindi riduci la griglia di conseguenza.
- **MONITORS (+):** punti in cui registrare temperatura e velocità a ogni
  iterazione; servono anche al controllo di convergenza.

### 5.5 Calcolo e convergenza

- **F5**: si apre la scheda *Convergence* con i residui di massa, quantità
  di moto ed energia in scala logaritmica e la linea del valore obiettivo.
- Il calcolo si ferma quando:
  - i tre residui scendono sotto la tolleranza, oppure
  - le Tj, i punti di monitoraggio e la cella più calda variano meno di
    max(0,05 K; 0,2 % del loro aumento) in 50 iterazioni (*temperatures
    settled*), oppure
  - si raggiunge *Max iterations*.
- Se il calcolo **non converge** (*NOT converged*), FerroTherm usa la media
  dei campi delle ultime iterazioni e indica di quanto oscillano le
  temperature (± metà del valore riportato). Rimedi: ridurre il
  rilassamento (*Relaxation u / p*, es. 0,3 / 0,5), aumentare le
  iterazioni, raffinare la griglia o fare un calcolo transitorio.

### 5.6 Visualizzare il flusso

- In alto scegli **Speed** (velocità dell'aria) o **Pressure** (pressione
  rispetto all'ambiente): si attiva automaticamente una sezione.
- *Section* + asse + cursore spostano il piano di taglio; **Vectors** mostra
  le frecce della velocità sul piano (*n* = numero di frecce per lato).
- In *Convergence* trovi la portata di ogni ventola (m³/s e CFM) con la
  pressione del punto di lavoro, la portata e il calore portato via da ogni
  apertura, e i valori dei punti di monitoraggio.

---

## 6. Tutorial C — importare la meccanica da STEP

1. *File → Import CAD…* → file `.step`/`.stp`. Il **primo** import prepara
   il motore OpenCascade (circa 30 s, poi resta in memoria cache).
2. La finestra elenca i corpi con nome, dimensioni e numero di triangoli.
   Negli assiemi esportati dai CAD elettronici i nomi sono i riferimenti dei
   componenti (C1, U5, …).
3. **Shape:**
   - *Bounding box per body*: ogni corpo diventa il suo parallelepipedo
     d'ingombro (veloce, adatto a pareti e coperchi);
   - *Voxelised shape*: il corpo viene ricostruito con cubetti della
     dimensione scelta e poi fusi in blocchi. Usa circa ¼ del dettaglio più
     piccolo che ti interessa.
4. **Material**, **Move by** (traslazione), **Skip bodies below** (scarta i
   corpi minuscoli come viti e pin), **Boards** (i corpi chiamati
   PCB/board diventano una scheda multistrato con 4 strati da 1 oz al 30 %:
   valore indicativo, segnalato nei messaggi).
5. *Add to model*.

La geometria di FerroTherm è fatta di blocchi allineati agli assi: le
superfici inclinate o curve diventano a gradini. Per schede e componenti
con potenza è meglio importare i dati ECAD (tutorial A) e usare lo STEP solo
per la meccanica.

---

## 7. Componenti DELPHI multi-nodo

Seleziona un componente e, in fondo al pannello, scegli **Multi-node
(DELPHI)**. Viene creata la topologia tipica (giunzione, top interno/esterno,
bottom interno/esterno, lati, terminali) con resistenze di **10 K/W
segnaposto**: sostituiscile con i valori forniti dal produttore del
componente. Puoi aggiungere o togliere nodi e collegamenti e scegliere a
quale superficie è attaccato ogni nodo. Dopo il calcolo il pannello mostra
il calore uscente da ogni superficie.

---

## 8. Esportare

- **File → Export geometry:** STEP (solidi AP214), STL oppure IDF 3.0 (board
  `.emn` + libreria `.emp`) per riportare il modello nel CAD meccanico o
  elettronico.
- **File → Export results:** VTK per ParaView (temperatura, velocità,
  modulo della velocità, pressione, materiale, densità di potenza), report
  Markdown e JSON, CSV di componenti, blocchi e, nei transitori, della Tj nel
  tempo.

---

## 9. Riga di comando

Utile per automatizzare o per i calcoli lunghi. Su Windows il programma è
`ferrotherm.exe`.

```bash
ferrotherm cad-info scheda.kicad_pcb               # cosa contiene un file CAD
ferrotherm import scheda.kicad_pcb -o box.toml --power potenze.csv --tile 10
ferrotherm import meccanica.step --into box.toml --shape voxel:1 --material al-6061-t6
ferrotherm import gerber.zip --into box.toml --origin 10,10,10
ferrotherm check box.toml                          # verifica il modello e la griglia
ferrotherm run box.toml -o risultati               # risolve e scrive i risultati
ferrotherm export box.toml box.step                # anche .stl e .emn
ferrotherm materials | packages | finishes | environments | deratings
ferrotherm pcb --thickness 1.6 --layers 8 --oz 1 --coverage 0.6
```

`ferrotherm run` termina con codice **2** se un componente supera il suo
limite di Tj: si può usare come controllo automatico.

I file caso (`.toml`) sono testo: si possono scrivere a mano, confrontare
con Git e aprire indifferentemente dall'interfaccia o dalla riga di comando.

---

## 10. Librerie

*Libraries → Browse libraries* mostra materiali, finiture, package,
ambienti e regole di derating, ciascuno con la sua fonte (ASM Handbook,
Incropera, MIL-HDBK-217F, NSWC Crane SD-18, MIL-STD-810H…). Per aggiungere i
tuoi materiali o componenti: *Libraries → Add a library file* con un file
JSON come questo:

```json
{ "materials": [
  { "id": "gap-pad-x", "name": "Gap pad X, 5 W/mK", "k": 5.0,
    "density": 3100, "cp": 1000, "source": "Datasheet fornitore rev C" }
]}
```

Una voce con un `id` già esistente sostituisce quella predefinita.

---

## 11. Consigli e limiti

- **Studio di griglia:** confronta sempre almeno due griglie.
- **Bilancio energetico:** potenza entrante e calore uscente devono
  coincidere; lo sbilancio è nella riga riassuntiva dei risultati.
- **Turbolenza:** solo LVEL o laminare; niente k-ε per ora.
- **Geometria:** solo blocchi allineati agli assi.
- **Calcolo:** solo su CPU (multithread), niente GPU.
- **IPC-2581 e ODB++:** verificati su file con la struttura di quelli
  esportati da KiCad; segnala i file di altri CAD che non vengono letti.
- **Valori di libreria:** sono tipici da manuale; i θ dei componenti
  commerciali vanno presi dal datasheet.
- Prima di usare i risultati per una qualifica, verifica la revisione
  corrente delle norme citate e confronta con misure.
