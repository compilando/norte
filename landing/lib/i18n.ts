/**
 * Every word the page says, in both languages. The Spanish object is typed
 * against the English one, so a sentence added to one and forgotten in the
 * other does not build.
 */

export const LANGS = ["en", "es"] as const;
export type Lang = (typeof LANGS)[number];

/** The terminal scenes the tour walks, as named in scripts/landing-shots/scenes/tour.scene. */
export type Scene =
  | "panes"
  | "viewer"
  | "markdown"
  | "copy-dialog"
  | "timeline"
  | "goto"
  | "palette"
  | "terminal"
  | "settings"
  | "menu"
  | "help";

const en = {
  meta: {
    title: "norte — the open-source file commander for the agent era",
    description:
      "Two panes, a terminal and a desktop window over one asynchronous Rust core. Local, SFTP, FTP, S3 and archives, with a journal that undoes, a policy agents cannot skip, and no telemetry.",
  },
  nav: {
    links: [
      ["Tour", "#tour"],
      ["What's new", "#new"],
      ["Themes", "#themes"],
      ["Keys", "#keys"],
      ["Agents", "#agents"],
    ],
    download: "Download",
    other: "Español",
    otherHref: "/es",
  },
  hero: {
    badge: "open source",
    protocol: "protocol",
    title: ["Your files have a new sense of ", "direction."],
    lede: "norte is an orthodox file manager for the terminal and the desktop, over one asynchronous Rust core, with one governed door for your AI agents.",
    primary: "Install the alpha",
    secondary: "Read the docs",
    platforms: "Linux x86_64 · deb / rpm / AppImage · macOS & Windows from source",
    tabs: { terminal: "Terminal · ntc", window: "Window · norte-gui" },
    theme: "Theme",
    live: "Live capture",
    caption: "Not a mockup. Every screen on this page is norte itself, captured by a script from the current build.",
    play: "Play",
    pause: "Pause",
  },
  signals: [
    "Two panes",
    "Terminal + window",
    "Local",
    "SFTP",
    "FTP",
    "S3",
    "ZIP / TAR / RAR",
    "MCP",
    "Journal & undo",
    "Pause & resume",
    "7 keymaps",
    "10 themes",
    "WASM plugins",
    "No telemetry",
  ],
  duo: {
    eyebrow: "One core, two screens",
    title: "The terminal and the window are the same program.",
    body: "Same keys, same themes, same layout, same journal. Start in ntc over SSH, carry on in the window at your desk: the tabs, the directories and even what you had marked travel with you.",
    terminal: "ntc — in any terminal, core embedded, nothing to start first",
    window: "norte-gui — a native window that brings its own daemon",
    missing: "Window captures are taken on a machine with Xvfb: run just landing-shots.",
  },
  tour: {
    eyebrow: "A tour, in real captures",
    title: "Everything a file manager should have done years ago.",
    steps: [
      {
        scene: "panes",
        kicker: "Two panes",
        title: "One is where you are. The other is where things go.",
        body: "The orthodox model: no dialog ever asks where to. Marks, sizes, free space and the encoding of the listing are always on screen. Names are bytes, so 東京 and Tromsø are just names.",
        keys: ["Tab", "Insert", "F5"],
      },
      {
        scene: "viewer",
        kicker: "The viewer",
        title: "An image is shown as an image.",
        body: "F3 opens anything. Pictures render as colour cells in any terminal, Markdown as styled text, and binaries as hex. Previewers are WASM plugins you approve.",
        keys: ["F3"],
      },
      {
        scene: "markdown",
        kicker: "Read without leaving",
        title: "A README reads like a README.",
        body: "Headings, emphasis, code, lists and quotes, from a sandboxed previewer that never sees the path of what it renders.",
        keys: ["F3"],
      },
      {
        scene: "copy-dialog",
        kicker: "Operations",
        title: "Copies are tasks. You keep working.",
        body: "Every copy, move, delete, sync and index runs in the background with progress, a clean cancel, pause and resume, and a serial queue for spinning disks.",
        keys: ["F5", "Ctrl+Alt+K", "Ctrl+Alt+Q"],
      },
      {
        scene: "timeline",
        kicker: "The journal",
        title: "Everything that changed, and a way back.",
        body: "Every mutation — yours, the window's, an agent's — lands in a journal. The timeline shows it, and undoes back to any point without ever touching a file it did not make.",
        keys: ["Ctrl+P timeline"],
      },
      {
        scene: "goto",
        kicker: "Go anywhere",
        title: "Frequent places, bookmarks and history in one list.",
        body: "Type three letters and jump. Each suggestion is decoded with the encoding of the pane it came from.",
        keys: ["Ctrl+G"],
      },
      {
        scene: "palette",
        kicker: "The palette",
        title: "Every command, found by name.",
        body: "All {commands} commands are in the palette, the F9 menus and the help, each with the key your preset gives it.",
        keys: ["Ctrl+P", "F9"],
      },
      {
        scene: "terminal",
        kicker: "New · terminal in a panel",
        title: "A shell under your files.",
        body: "Ctrl+Alt+S opens a real terminal in the focused directory — colour, alternate screen, vim and less. Press it again to get the keyboard back; the shell keeps running.",
        keys: ["Ctrl+Alt+S"],
      },
      {
        scene: "settings",
        kicker: "Settings",
        title: "Every option, in sections, with real controls.",
        body: "Theme, keys, columns, stripes, icons, the status bar. Changes apply live and are written to the norte.toml you already have.",
        keys: ["F11"],
      },
      {
        scene: "help",
        kicker: "Help",
        title: "A manual that knows your keys.",
        body: "The help is written for your preset and your language, and every plugin brings its own page.",
        keys: ["F1"],
      },
    ] as { scene: Scene; kicker: string; title: string; body: string; keys: string[] }[],
  },
  news: {
    eyebrow: "New in {version}",
    title: "What landed since the last alpha.",
    items: [
      ["Terminal in a panel", "A shell below the listings, in both frontends, sharing one emulator.", "Ctrl+Alt+S"],
      ["Pause and resume", "A copy stops at the end of its chunk and carries on where it left off.", "Ctrl+Alt+K"],
      ["A queue for transfers", "One at a time on a spinning disk, reordered while they wait.", "Ctrl+Alt+Q"],
      ["Retry what failed", "The last failed transfer again, same options, one key.", "Ctrl+Alt+R"],
      ["Type to jump", "In the Krusader preset a letter jumps to the first name that starts with it.", "ADR 0155"],
      ["A window like VS Code", "Activity bar, panels you drag between edges, tabs, a marks ruler, custom title bar.", "ADR 0131–0138"],
      ["--lang es|en", "The language of one run, in all three binaries, without touching the config.", "ntc · norte · gui"],
      ["Owners by name", "Owner and group columns as ls -l shows them, and every attribute column sorts.", "ADR 0144–0145"],
      ["Search, narrowed", "Size, date, type, depth, hidden, links: ten filters on fs.search.", "protocol 0.81"],
      ["Status bar progress", "One light bar for running work, and a quiet ✓ when it is done.", "ADR 0146"],
    ] as [string, string, string][],
  },
  themes: {
    eyebrow: "Ten themes, both frontends",
    title: "Make it look like the rest of your desk.",
    body: "Catppuccin, Gruvbox, Nord, VS Code, two CRT phosphors and norte's own. One theme file paints the terminal and the window alike, follows your desktop's light and dark, and can be written from scratch.",
    pick: "Click one to try it in the hero.",
  },
  keys: {
    eyebrow: "Seven keymaps",
    title: "Your fingers already know it.",
    body: "Transcribed from the managers they are named after, not guessed. Every command exists in all seven, or the preset's header says why not.",
    command: "Command",
    commands: {
      "cursor.down": "Next row",
      "cursor.top": "First row",
      "nav.parent": "Parent folder",
      "pane.copy": "Copy",
      "pane.search": "Search",
      "pane.tab-new": "New tab",
      "pane.tab-next": "Next tab",
      "pane.swap": "Swap panes",
      "app.palette": "Palette",
      "app.quit": "Quit",
    } as Record<string, string>,
  },
  agents: {
    eyebrow: "Agents, governed",
    title: "Your AI works with your files. Never around them.",
    body: "Claude, Codex or any MCP client reaches your files through the same core the human frontends use: scoped access, approvals, expiring grants, attribution in the journal, and undo for an agent's whole session after it is gone.",
    points: [
      ["Plans, not actions", "An agent proposes a rename or an organization; you review the tree and approve it in one batch."],
      ["One gate", "The policy engine sits in the core. There is no side door for a plugin, an agent or a frontend."],
      ["Plugins ask first", "A WASM plugin arrives unapproved. It runs only after you grant what it asked for, and a changed binary asks again."],
    ] as [string, string][],
    grant: "Installing is not consenting: a plugin's capabilities are granted by a person.",
  },
  principles: {
    eyebrow: "Why norte",
    lead: "The file manager stopped evolving. Your work did not.",
    title: "Not a prettier explorer. ",
    muted: "A programmable, governed system for everything you keep.",
    items: [
      ["01", "One namespace", "Local disks, SFTP, FTP, S3 and archives behave like one filesystem."],
      ["02", "Work in motion", "Copies, comparisons, syncs and indexes are observable, cancellable tasks."],
      ["03", "Control stays human", "AI proposes, agents request, the core enforces — and every change leaves a way back."],
      ["04", "Nothing phones home", "No account, no cloud, no telemetry switch hidden in settings. AGPL, with permissive protocol and providers."],
    ] as [string, string, string][],
  },
  cta: {
    eyebrow: "Your filesystem, headed somewhere",
    title: "Ready to point north?",
    body: "Install the Linux alpha today, or build norte anywhere Rust runs. No account. No telemetry. Just your files, under control.",
    terminal: "Terminal · Linux x86_64",
    tui: "ntc — the file manager",
    cli: "norte — daemon, CLI, MCP",
    copy: "copy",
    copied: "copied",
    all: "All downloads",
    source: "Build from source",
    window: "The window · one bundle, three binaries",
    packages: [
      [".deb", "Debian · Ubuntu", "dpkg -i"],
      [".rpm", "Fedora · openSUSE", "dnf install"],
      [".AppImage", "Anything else", "chmod +x"],
    ] as [string, string, string][],
    carries: "Each package carries norte and ntc with it, so a clean install always has a daemon to talk to.",
    platforms: "Linux x86_64 today. macOS and Windows build from source; they ship the day releases run in CI.",
  },
  footer: {
    tagline: "The open-source file commander for people, terminals, windows and agents.",
    promise: "Built in Rust. No telemetry. Ever.",
    groups: [
      ["Product", [["Tour", "#tour"], ["What's new", "#new"], ["Themes", "#themes"], ["Keys", "#keys"], ["Download", "#download"]]],
      ["Build", [["Documentation", "docs"], ["Architecture map", "architecture"], ["Specification", "spec"], ["Decision records", "adr"], ["Plugin authoring", "plugins"]]],
      ["Open source", [["Source code", "repo"], ["Releases", "releases"], ["Changelog", "changelog"], ["Contributing", "contributing"], ["Security", "security"]]],
    ] as [string, [string, string][]][],
    license: "AGPL-3.0 · Protocol & providers MIT / Apache-2.0",
  },
};

export type Copy = typeof en;

const es: Copy = {
  meta: {
    title: "norte — el gestor de ficheros libre para la era de los agentes",
    description:
      "Dos paneles, una terminal y una ventana de escritorio sobre un único núcleo asíncrono en Rust. Local, SFTP, FTP, S3 y archivos comprimidos, con un diario que deshace, una política que los agentes no se saltan y sin telemetría.",
  },
  nav: {
    links: [
      ["Recorrido", "#tour"],
      ["Novedades", "#new"],
      ["Temas", "#themes"],
      ["Teclas", "#keys"],
      ["Agentes", "#agents"],
    ],
    download: "Descargar",
    other: "English",
    otherHref: "/",
  },
  hero: {
    badge: "código abierto",
    protocol: "protocolo",
    title: ["Tus ficheros tienen un nuevo ", "norte."],
    lede: "norte es un gestor de ficheros ortodoxo para la terminal y el escritorio, sobre un único núcleo asíncrono en Rust, con una sola puerta —vigilada— para tus agentes de IA.",
    primary: "Instalar la alfa",
    secondary: "Leer la documentación",
    platforms: "Linux x86_64 · deb / rpm / AppImage · macOS y Windows desde el código",
    tabs: { terminal: "Terminal · ntc", window: "Ventana · norte-gui" },
    theme: "Tema",
    live: "Captura real",
    caption: "Nada de maquetas. Cada pantalla de esta página es norte, capturado por un script a partir de la versión actual.",
    play: "Reproducir",
    pause: "Pausa",
  },
  signals: [
    "Dos paneles",
    "Terminal + ventana",
    "Local",
    "SFTP",
    "FTP",
    "S3",
    "ZIP / TAR / RAR",
    "MCP",
    "Diario y deshacer",
    "Pausar y reanudar",
    "7 teclados",
    "10 temas",
    "Plugins WASM",
    "Sin telemetría",
  ],
  duo: {
    eyebrow: "Un núcleo, dos pantallas",
    title: "La terminal y la ventana son el mismo programa.",
    body: "Mismas teclas, mismos temas, misma disposición, mismo diario. Empieza en ntc por SSH y sigue en la ventana en tu mesa: las pestañas, los directorios y hasta lo que tenías marcado viajan contigo.",
    terminal: "ntc — en cualquier terminal, con el núcleo dentro: nada que arrancar antes",
    window: "norte-gui — una ventana nativa que trae su propio demonio",
    missing: "Las capturas de la ventana se sacan en una máquina con Xvfb: ejecuta just landing-shots.",
  },
  tour: {
    eyebrow: "Un recorrido, con capturas reales",
    title: "Todo lo que un gestor de ficheros debió hacer hace años.",
    steps: [
      {
        scene: "panes",
        kicker: "Dos paneles",
        title: "Uno es donde estás. El otro, adonde van las cosas.",
        body: "El modelo ortodoxo: ningún diálogo pregunta «¿adónde?». Marcas, tamaños, espacio libre y la codificación del listado siempre a la vista. Los nombres son bytes, así que 東京 y Tromsø son solo nombres.",
        keys: ["Tab", "Insert", "F5"],
      },
      {
        scene: "viewer",
        kicker: "El visor",
        title: "Una imagen se ve como una imagen.",
        body: "F3 abre cualquier cosa. Las fotos se pintan con celdas de color en cualquier terminal, el Markdown como texto con estilo y los binarios en hexadecimal. Los previsualizadores son plugins WASM que tú apruebas.",
        keys: ["F3"],
      },
      {
        scene: "markdown",
        kicker: "Leer sin salir",
        title: "Un README se lee como un README.",
        body: "Títulos, énfasis, código, listas y citas, desde un previsualizador aislado que nunca conoce la ruta de lo que pinta.",
        keys: ["F3"],
      },
      {
        scene: "copy-dialog",
        kicker: "Operaciones",
        title: "Copiar es una tarea. Tú sigues trabajando.",
        body: "Cada copia, movimiento, borrado, sincronización e índice corre en segundo plano con progreso, cancelación limpia, pausa y reanudación, y una cola en serie para discos mecánicos.",
        keys: ["F5", "Ctrl+Alt+K", "Ctrl+Alt+Q"],
      },
      {
        scene: "timeline",
        kicker: "El diario",
        title: "Todo lo que cambió, y el camino de vuelta.",
        body: "Cada cambio —tuyo, de la ventana, de un agente— queda en un diario. La línea de tiempo lo enseña y deshace hasta cualquier punto sin tocar nunca un fichero que no creó.",
        keys: ["Ctrl+P timeline"],
      },
      {
        scene: "goto",
        kicker: "Ir a cualquier sitio",
        title: "Sitios frecuentes, favoritos e historia en una lista.",
        body: "Teclea tres letras y salta. Cada sugerencia se decodifica con la codificación del panel del que viene.",
        keys: ["Ctrl+G"],
      },
      {
        scene: "palette",
        kicker: "La paleta",
        title: "Cualquier orden, por su nombre.",
        body: "Las {commands} órdenes están en la paleta, en los menús de F9 y en la ayuda, cada una con la tecla que le da tu preset.",
        keys: ["Ctrl+P", "F9"],
      },
      {
        scene: "terminal",
        kicker: "Nuevo · terminal en un panel",
        title: "Un shell debajo de tus ficheros.",
        body: "Ctrl+Alt+S abre una terminal de verdad en el directorio del panel: color, pantalla alternativa, vim y less. Púlsalo otra vez y recuperas el teclado; el shell sigue vivo.",
        keys: ["Ctrl+Alt+S"],
      },
      {
        scene: "settings",
        kicker: "Ajustes",
        title: "Cada opción, por secciones, con controles de verdad.",
        body: "Tema, teclas, columnas, bandas, iconos, la barra de estado. Los cambios se aplican al momento y se escriben en el norte.toml que ya tienes.",
        keys: ["F11"],
      },
      {
        scene: "help",
        kicker: "Ayuda",
        title: "Un manual que sabe tus teclas.",
        body: "La ayuda está escrita para tu preset y tu idioma, y cada plugin trae su propia página.",
        keys: ["F1"],
      },
    ],
  },
  news: {
    eyebrow: "Nuevo en {version}",
    title: "Lo que ha llegado desde la última alfa.",
    items: [
      ["Terminal en un panel", "Un shell bajo los listados, en los dos frontends, con un único emulador.", "Ctrl+Alt+S"],
      ["Pausar y reanudar", "Una copia para al final de su bloque y sigue donde lo dejó.", "Ctrl+Alt+K"],
      ["Cola de transferencias", "De una en una en un disco mecánico, reordenables mientras esperan.", "Ctrl+Alt+Q"],
      ["Repetir lo que falló", "La última transferencia fallida otra vez, con sus opciones, en una tecla.", "Ctrl+Alt+R"],
      ["Teclear para saltar", "En el preset Krusader, una letra salta al primer nombre que empieza por ella.", "ADR 0155"],
      ["Una ventana como VS Code", "Barra de actividad, paneles que se arrastran entre bordes, pestañas, regla de marcas, barra de título propia.", "ADR 0131–0138"],
      ["--lang es|en", "El idioma de una ejecución, en los tres binarios, sin tocar la configuración.", "ntc · norte · gui"],
      ["Dueños por nombre", "Columnas de dueño y grupo como las enseña ls -l, y toda columna de atributo ordena.", "ADR 0144–0145"],
      ["Búsqueda acotada", "Tamaño, fecha, tipo, profundidad, ocultos, enlaces: diez filtros en fs.search.", "protocolo 0.81"],
      ["Progreso en la barra", "Una barra ligera para el trabajo en curso, y un ✓ discreto al terminar.", "ADR 0146"],
    ],
  },
  themes: {
    eyebrow: "Diez temas, dos frontends",
    title: "Que se parezca al resto de tu escritorio.",
    body: "Catppuccin, Gruvbox, Nord, VS Code, dos fósforos CRT y el propio de norte. Un solo fichero de tema pinta la terminal y la ventana, sigue el claro y oscuro de tu escritorio y se puede escribir desde cero.",
    pick: "Pulsa uno para probarlo arriba.",
  },
  keys: {
    eyebrow: "Siete teclados",
    title: "Tus dedos ya se lo saben.",
    body: "Transcritos de los gestores cuyo nombre llevan, no inventados. Cada orden existe en los siete, o la cabecera del preset explica por qué no.",
    command: "Orden",
    commands: {
      "cursor.down": "Fila siguiente",
      "cursor.top": "Primera fila",
      "nav.parent": "Carpeta padre",
      "pane.copy": "Copiar",
      "pane.search": "Buscar",
      "pane.tab-new": "Pestaña nueva",
      "pane.tab-next": "Pestaña siguiente",
      "pane.swap": "Intercambiar paneles",
      "app.palette": "Paleta",
      "app.quit": "Salir",
    },
  },
  agents: {
    eyebrow: "Agentes, con reglas",
    title: "Tu IA trabaja con tus ficheros. Nunca a sus espaldas.",
    body: "Claude, Codex o cualquier cliente MCP llegan a tus ficheros por el mismo núcleo que usan los frontends humanos: acceso acotado, aprobaciones, permisos que caducan, autoría en el diario y deshacer la sesión entera de un agente cuando ya se ha ido.",
    points: [
      ["Planes, no actos", "Un agente propone un renombrado o una organización; tú revisas el árbol y lo apruebas en un solo lote."],
      ["Una sola puerta", "El motor de políticas vive en el núcleo. No hay puerta lateral para un plugin, un agente ni un frontend."],
      ["Los plugins preguntan", "Un plugin WASM llega sin aprobar. Solo corre cuando concedes lo que pidió, y un binario cambiado vuelve a preguntar."],
    ],
    grant: "Instalar no es consentir: los permisos de un plugin los concede una persona.",
  },
  principles: {
    eyebrow: "Por qué norte",
    lead: "El gestor de ficheros dejó de evolucionar. Tu trabajo, no.",
    title: "No es un explorador más bonito. ",
    muted: "Es un sistema programable y gobernado para todo lo que guardas.",
    items: [
      ["01", "Un solo espacio de nombres", "Discos locales, SFTP, FTP, S3 y archivos comprimidos se comportan como un único sistema de ficheros."],
      ["02", "Trabajo en marcha", "Copias, comparaciones, sincronizaciones e índices son tareas que se observan y se cancelan."],
      ["03", "El control es humano", "La IA propone, los agentes piden, el núcleo decide, y cada cambio deja un camino de vuelta."],
      ["04", "Nada llama a casa", "Sin cuenta, sin nube, sin interruptor de telemetría escondido en los ajustes. AGPL, con protocolo y proveedores permisivos."],
    ],
  },
  cta: {
    eyebrow: "Tu sistema de ficheros, con rumbo",
    title: "¿Listo para apuntar al norte?",
    body: "Instala hoy la alfa para Linux, o compila norte donde corra Rust. Sin cuenta. Sin telemetría. Solo tus ficheros, bajo control.",
    terminal: "Terminal · Linux x86_64",
    tui: "ntc — el gestor de ficheros",
    cli: "norte — demonio, CLI, MCP",
    copy: "copiar",
    copied: "copiado",
    all: "Todas las descargas",
    source: "Compilar desde el código",
    window: "La ventana · un paquete, tres binarios",
    packages: [
      [".deb", "Debian · Ubuntu", "dpkg -i"],
      [".rpm", "Fedora · openSUSE", "dnf install"],
      [".AppImage", "Cualquier otra", "chmod +x"],
    ],
    carries: "Cada paquete lleva norte y ntc dentro, así que una instalación limpia siempre tiene un demonio con el que hablar.",
    platforms: "Hoy, Linux x86_64. macOS y Windows compilan desde el código; se publicarán el día que las versiones se construyan en CI.",
  },
  footer: {
    tagline: "El gestor de ficheros libre para personas, terminales, ventanas y agentes.",
    promise: "Hecho en Rust. Sin telemetría. Nunca.",
    groups: [
      ["Producto", [["Recorrido", "#tour"], ["Novedades", "#new"], ["Temas", "#themes"], ["Teclas", "#keys"], ["Descargar", "#download"]]],
      ["Construir", [["Documentación", "docs"], ["Mapa de arquitectura", "architecture"], ["Especificación", "spec"], ["Decisiones (ADR)", "adr"], ["Escribir plugins", "plugins"]]],
      ["Código abierto", [["Código fuente", "repo"], ["Versiones", "releases"], ["Cambios", "changelog"], ["Contribuir", "contributing"], ["Seguridad", "security"]]],
    ],
    license: "AGPL-3.0 · Protocolo y proveedores MIT / Apache-2.0",
  },
};

export const COPY: Record<Lang, Copy> = { en, es };

/** `{name}` placeholders, filled from `vars`. */
export function fill(text: string, vars: Record<string, string | number>): string {
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m));
}
