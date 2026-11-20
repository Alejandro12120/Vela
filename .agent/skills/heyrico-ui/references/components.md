# Patrones de componentes

## Navegación y cabecera

- Separar identidad del espacio, navegación principal y grupos secundarios. Introducir divisores solo cuando ayuden a reconocer los grupos.
- Mantener el icono y el texto alineados; situar contadores y atajos al extremo opuesto. Usar atajos solo si funcionan.
- Representar la selección con un fondo neutro ligeramente distinto. Conservar una indicación adicional cuando haga falta para distinguirla de hover.
- Mantener título, ruta o contexto cerca del contenido al que pertenecen. Agrupar filtros y acciones en una barra breve.
- En móvil, convertir la navegación en un panel accesible que pueda abrirse y cerrarse; conservar el contexto de la página.

## Botones y pestañas

- Utilizar unos 32 px de altura visual en escritorio como punto de partida. Ampliar el área interactiva en táctil.
- Dar a la acción principal un contraste claro. Mantener acciones secundarias neutras y acciones de texto discretas.
- Introducir iconos solo si ayudan a reconocer la acción. Alinear iconos de una misma familia y tamaño.
- Elegir pestañas con subrayado para secciones de contenido; utilizar un fondo seleccionado para controles segmentados o modos de una misma vista.
- Aplicar la semántica y el comportamiento de teclado correspondientes al patrón elegido; mantener visibles los contadores subordinados a la etiqueta.

## Tablas y listas

- Utilizar una superficie continua y encabezados de poco peso. Empezar con filas de unos 40 px como propuesta ajustable.
- Colocar el identificador principal al inicio de la fila. Reservar ancho para estados y acciones; alinear cifras por el extremo derecho.
- Utilizar separadores horizontales suaves y hover discreto. Mantener foco y selección perceptibles.
- Incluir logos o avatares pequeños cuando aporten reconocimiento. Limitar las etiquetas a los estados que merezcan mostrarse.
- Permitir truncado en descripciones secundarias; ofrecer acceso al contenido completo. Mantener identificables el nombre principal y los datos necesarios para decidir.
- Conservar el encabezado o contexto cuando el volumen justifique desplazamiento. Mostrar recuentos o paginación solo cuando existan datos que los respalden.
- En móvil, elegir explícitamente entre una tabla con desplazamiento local y una lista adaptada. Mantener la relación entre encabezados y valores.

## Menús, selectores y popovers

- Utilizar un panel compacto con radio mayor que el de las filas interiores, borde suave y sombra suficiente para separarlo del contenido.
- Agrupar opciones relacionadas, con icono a la izquierda y marca de selección a la derecha. Diferenciar el foco de teclado.
- Situar acciones de creación junto al grupo al que afectan. Separar ajustes y acciones de salida; utilizar rojo para acciones destructivas reales.
- Anclar el panel a su disparador, limitar su altura y mantenerlo dentro de la ventana. Gestionar apertura, cierre, foco y Escape.
- Utilizar la implementación accesible existente. Para selecciones sencillas, considerar un select nativo; para buscadores complejos, utilizar el patrón de combobox adecuado.

## Formularios y ajustes

- Agrupar campos por la decisión que ayudan a tomar. Mantener etiquetas visibles, ayuda breve y errores junto al campo.
- Utilizar el espacio para separar secciones y una cuadrícula que pueda volver a una columna en ventanas estrechas.
- Distinguir controles editables de texto informativo. Evitar que un borde demasiado tenue sea su única señal de interacción.
- Aplicar switches a opciones que cambian inmediatamente y casillas a selecciones que se confirman juntas, según el comportamiento real del producto.
- Mostrar el estado de guardado o resultado cuando corresponda. Conservar los datos introducidos si hay un error recuperable.

## Analítica

- Dar protagonismo al valor, periodo y comparación útiles. Mantener unidades y criterios de cálculo claros.
- Agrupar métricas relacionadas en un mismo bloque cuando compartan una gráfica. Evitar una sucesión de tarjetas KPI sin relación con la tarea.
- Utilizar ejes y rejillas discretos, líneas finas y color localizado en las series. Asegurar que las series se puedan distinguir por etiquetas, trazos o símbolos además del color.
- Usar colores más expresivos en pequeños gráficos de barras o segmentos cuando faciliten comparar categorías. Mantener el marco del gráfico neutro.
- Conservar escalas, unidades y proporciones correctas. No fabricar tendencias a partir de números que no las respalden.

## Aplicaciones de IA

- Organizar conversación, actividad, fuentes y resultados según la tarea. Abrir un panel lateral de detalle cuando tenga suficiente contenido propio.
- Diseñar el composer como un control claro, con entrada, adjuntos y acción de envío reconocibles. Mostrar opciones avanzadas de forma progresiva.
- Agrupar archivos, referencias y herramientas en chips o filas compactas. Evitar decorar cada párrafo con una tarjeta.
- Mostrar un estado de ejecución fiel al proceso. Distinguir preparación, ejecución, finalización y error cuando existan.
- Hacer que las acciones visibles sobre resultados funcionen. En prototipos, indicar los datos o comportamientos simulados sin dar a entender conexiones reales.

## Vacío, carga, error y contenido largo

- En vacío, explicar qué falta y ofrecer una acción pertinente. Utilizar ilustración solo si aporta algo al contexto.
- En carga, conservar las dimensiones aproximadas del resultado para limitar saltos de posición.
- En error, describir el problema concreto y ofrecer recuperación cuando exista. Evitar mensajes genéricos si se conoce la causa.
- Probar etiquetas largas, filas con varios estados y menús extensos. Permitir que el diseño absorba el contenido antes de ocultarlo.
