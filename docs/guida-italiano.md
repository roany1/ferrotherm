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
     zona di sole piste. Le tessere sul bordo seguono il contorno reale
     della scheda (forme a L, smussi, asole e fori di fissaggio grandi
     restano aria);
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
   **k–ε** (con wall function scalabili per attrito e scambio termico) è
   indicato per flussi forzati pienamente turbolenti, con ventole e
   condotti; in quel caso compare anche l'intensità turbolenta dell'aria in
   ingresso (default 5 %). Su flussi lenti a basso Reynolds e griglie
   grossolane k–ε sottostima lo scambio termico: lì resta preferibile LVEL
   (vedi i limiti, §12).

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

### 6.1 Semplificare la meccanica (defeaturing)

Prima di *Add to model* apri la sezione **Simplify geometry
(defeaturing)** della stessa finestra. Lavora sui solidi CAD esatti con
OpenCascade, quindi prima che diventino blocchi o voxel:

- **Close holes ≤ Ø**: chiude fori passanti, ciechi, svasati e lamati
  (il fondo del foro cieco e gli svasi vengono riconosciuti insieme al foro);
- **Remove bosses / pins ≤ Ø**: toglie torrette, perni e distanziali, anche
  forati;
- **Remove fillets ≤ R** e **Remove chamfers ≤ larghezza**: riporta gli
  spigoli vivi, così i voxel non creano gradini inutili;
- **Drop small parts ≤ mm³**: scarta viti, dadi, rondelle, rivetti;
- **Box nearly-box parts**: sostituisce con il parallelepipedo d'ingombro i
  pezzi che lo riempiono almeno per la frazione indicata.

*Simplify* esegue in background e sostituisce il modello mostrato con quello
semplificato; la tabella riporta per ogni parte cosa è stato tolto, le facce
prima/dopo e il volume. *Save simplified STEP…* salva il risultato. Nomi e
posizioni dei pezzi dell'assieme restano quelli originali, e ogni pezzo
ripetuto viene semplificato una volta sola. Se OpenCascade non riesce a
togliere una certa feature, questa resta e viene contata come *kept*: il
pezzo non viene mai lasciato rotto.

Da riga di comando:

```bash
ferrotherm defeature chassis.step semplice.step --holes 8 --fillets 3 --chamfers 2 --small-parts 50
```

Vengono riconosciute le facce analitiche (cilindri, coni, tori, piani): un
foro esportato come superficie B-spline non viene riconosciuto.

La geometria di FerroTherm è fatta di blocchi allineati agli assi: le
superfici inclinate o curve diventano a gradini. Per schede e componenti
con potenza è meglio importare i dati ECAD (tutorial A) e usare lo STEP solo
per la meccanica.

---

## 7. Importare un modello Cadence Celsius EC Solver

*File → Import CAD…* e scegli il file `.equipment` (oppure
`ferrotherm import modello.equipment -o modello.toml`). Vengono letti:
contenitore e pareti, ambiente (temperatura, coefficiente di scambio
esterno, irraggiamento), fluido, impostazioni del solutore (flusso,
gravità, turbolenza, iterazioni), materiali con i loro valori, schede
multistrato, componenti 2R (θJC/θJB) o a blocco con la potenza, dissipatori
estrusi (con i tagli nelle alette), gap pad, ostacoli (parallelepipedi e
solidi CAD, voxelizzati) e gruppi. Per i modelli da rack: ventole e vent
nelle pareti (con curva P–Q in m³/s o cfm e perdita della lamiera forata),
alimentatori, Chassis_Bay con i server negli slot (anche ruotati),
Card_Bay con schede e backplane, Drive_Bay con i dischi, socket dei chip
(con il chip centrato) e socket delle schede figlie (DIMM). Gli oggetti
segnati "non installati" in Celsius vengono saltati.

- **Open as new model** sostituisce il modello corrente; **Add to current
  model** aggiunge solo oggetti e materiali.
- Celsius ha l'asse Y verso l'alto; FerroTherm usa x = X, y = profondità − Z,
  z = Y.
- Schede ruotate (per esempio verticali) e i componenti montati sopra
  vengono posizionati con la rotazione della scheda; la normale della scheda
  diventa l'asse di montaggio dei componenti. Rotazioni su un solo asse,
  posizione dei componenti (angolo basso dell'impronta, qualunque sia
  l'Origin_Point mostrato) e posizione dei solidi CAD nei gruppi sono
  verificate confrontando un modello 2024.36 con l'export STEP fatto da
  Celsius (19 oggetti coincidenti entro 1 µm).
- Non ancora verificati, e quindi segnalati: rotazioni che combinano più
  assi, schede inclinate di angoli non multipli di 90° (importate come
  bounding box), componenti ruotati sulla scheda. Gli oggetti che finiscono
  fuori dal contenitore vengono elencati negli avvisi: controllali.
- Se in Celsius l'irraggiamento è attivo, viene attivato `[radiation]` con
  le emissività superficiali dei materiali Celsius; le aperture irraggiano
  verso l'ambiente.
- I solidi CAD formati da più corpi che si toccano (pareti e guide di un
  contenitore conduction-cooled) vengono voxelizzati correttamente. Di
  default l'import usa voxel da 1 mm: voxel più grossi spostano le
  geometrie sottili (guide delle schede) e possono cambiare il risultato;
  la bounding box riempirebbe di alluminio un contenitore cavo.
- I componenti "Detailed" (package con die, substrato, sfere…) vengono
  importati come corpo del package più le sue parti interne, ognuna con il
  suo materiale e la sua potenza: la temperatura del die è risolta e compare
  nella tabella dei blocchi (`U6/Die`). Il substrato diventa una scheda
  multistrato con i suoi strati di rame; le schiere di sfere di saldatura
  (centinaia di cilindri da pochi decimi di mm, impossibili da meshare una
  per una) diventano uno strato equivalente con la conducibilità giusta
  attraverso lo spessore (frazione di lega) e nel piano. Sui modelli reali
  2024.36 le posizioni delle parti sono riferite all'angolo del package;
  l'import prova comunque anche scheda e chassis e scrive negli avvisi
  quale ha usato. Se il package è ruotato sulla scheda o contiene solidi
  CAD, torna al blocco unico con la potenza totale.
- Il **numero minimo di celle** di Celsius (5 × 5 di default su ogni
  oggetto) viene importato come `min_cells` sul blocco. È importante: con
  celle da 3,5 mm un die da 3 mm è una cella sola e la sua temperatura
  esce circa 10 K più alta. Fa crescere la griglia (su test_paper2 da
  0,65 a 1,3 milioni di celle).
- Le schede poligonali mantengono il loro contorno (`Outline`): appartengono
  alla scheda solo le celle con il centro dentro il poligono.
- **Convenzioni dei modelli da rack**, verificate sull'export STEP del
  tutorial "Full Enclosure" di Celsius (tutte le 1300 parti solide, le 16
  ventole e i 36 vent coincidono entro 2 µm): la parete Front è a Z
  massimo e Rear a Z = 0; gli offset dei vent sono l'angolo, quelli delle
  ventole il centro; il piano della ventola è sulla faccia interna del suo
  box; il calore dichiarato della ventola va nel mozzo; i vent sulle pareti
  esterne diventano aperture con la perdita della lamiera forata, quelli
  interni resistenze piane. L'interno dell'alimentatore (blocco interno
  percentuale) è reso con 10 piastre riscaldate lungo il flusso: è
  un'approssimazione. Le pareti dei bay, che Celsius tratta a spessore
  nullo, hanno qui il loro spessore verso l'interno. I TIM senza spessore
  non sono modellati: l'avviso riporta il ΔT che trascurano (P·R).
- **Griglia nei modelli grandi:** la griglia di FerroTherm è conforme (ogni
  piano attraversa tutto il dominio). Un rack ha migliaia di piani: usa
  *Merge planes closer than* (`merge_mm`, sotto lo spessore delle parti
  più sottili, per esempio 0,9 mm con alette da 1 mm) e *Cells through
  boards* (`pcb_cells` = 1 dove le schede non sono il percorso termico
  principale). Sul tutorial da rack si passa da 12,8 a 2,4 milioni di
  celle.
- Gli oggetti non ancora supportati (cold plate, sensori, ventole e vent
  isolati non su una parete…) sono elencati negli avvisi della finestra di
  import.
- La turbolenza k-ε di Celsius diventa k–ε; gli altri modelli diventano
  LVEL (segnalato).
- Il formato è stato ricostruito da file salvati con le versioni 17.44 e
  2024.36: non è una specifica ufficiale.

## 8. Componenti DELPHI multi-nodo

Seleziona un componente e, in fondo al pannello, scegli **Multi-node
(DELPHI)**. Viene creata la topologia tipica (giunzione, top interno/esterno,
bottom interno/esterno, lati, terminali) con resistenze di **10 K/W
segnaposto**: sostituiscile con i valori forniti dal produttore del
componente. Puoi aggiungere o togliere nodi e collegamenti e scegliere a
quale superficie è attaccato ogni nodo. Dopo il calcolo il pannello mostra
il calore uscente da ogni superficie.

---

## 9. Esportare

- **File → Export geometry:** STEP (solidi AP214), STL oppure IDF 3.0 (board
  `.emn` + libreria `.emp`) per riportare il modello nel CAD meccanico o
  elettronico.
- **File → Export results:** VTK per ParaView (temperatura, velocità,
  modulo della velocità, pressione, materiale, densità di potenza), report
  Markdown e JSON, CSV di componenti, blocchi e, nei transitori, della Tj nel
  tempo.

---

## 10. Riga di comando

Utile per automatizzare o per i calcoli lunghi. Su Windows il programma è
`ferrotherm.exe`.

```bash
ferrotherm cad-info scheda.kicad_pcb               # cosa contiene un file CAD
ferrotherm import scheda.kicad_pcb -o box.toml --power potenze.csv --tile 10
ferrotherm import meccanica.step --into box.toml --shape voxel:1 --material al-6061-t6
ferrotherm import gerber.zip --into box.toml --origin 10,10,10
ferrotherm check box.toml                          # verifica il modello e la griglia
ferrotherm run box.toml -o risultati               # risolve e scrive i risultati
ferrotherm defeature meccanica.step semplice.step --holes 8 --fillets 3
ferrotherm export box.toml box.step                # anche .stl e .emn
ferrotherm materials | packages | finishes | environments | deratings
ferrotherm pcb --thickness 1.6 --layers 8 --oz 1 --coverage 0.6
```

`ferrotherm run` termina con codice **2** se un componente supera il suo
limite di Tj e con codice **3** se il calcolo del flusso non è arrivato a
convergenza: si può usare come controllo automatico. Un calcolo stazionario
con flusso si ferma prima del numero massimo di iterazioni solo se le
temperature chiave sono stabili *e* i residui di massa ed energia sono sotto
dieci volte la `tolerance`.

I file caso (`.toml`) sono testo: si possono scrivere a mano, confrontare
con Git e aprire indifferentemente dall'interfaccia o dalla riga di comando.

---

## 11. Librerie

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

## 12. Consigli e limiti

- **Studio di griglia:** confronta sempre almeno due griglie.
- **Bilancio energetico:** potenza entrante e calore uscente devono
  coincidere; lo sbilancio è nella riga riassuntiva dei risultati.
- **Turbolenza:** LVEL, k–ε standard o laminare (niente k-ω/SST per ora).
  k–ε con wall function è pensato per flussi forzati turbolenti: in un canale
  dà C_f −11 % e Nu +4 % rispetto alle correlazioni di Dean e Gnielinski.
  Su un blocco scaldato a 3 m/s (Re ≈ 5600, di fatto laminare) sottostima lo
  scambio con celle grossolane (587 °C con celle da 2,5 mm, 340 °C con 1 mm;
  LVEL 245 / 302 °C): per l'elettronica a bassa velocità resta LVEL.
- **Geometria:** blocchi allineati agli assi; i contorni poligonali (schede
  di forma qualsiasi, asole) sono risolti al centro delle celle.
- **Calcolo:** solo su CPU (multithread), niente GPU.
- **IPC-2581 e ODB++:** verificati su file con la struttura di quelli
  esportati da KiCad; segnala i file di altri CAD che non vengono letti.
- **Valori di libreria:** sono tipici da manuale; i θ dei componenti
  commerciali vanno presi dal datasheet.
- Prima di usare i risultati per una qualifica, verifica la revisione
  corrente delle norme citate e confronta con misure.
