---
name: heyrico-ui
description: >-
  Diseñar, implementar y revisar interfaces de producto compactas inspiradas en
  @_heyrico: tipografía pequeña, superficies neutras, bordes sutiles, iconos finos
  y alta densidad bien organizada. Usar cuando se pida este estilo, Heyrico UI,
  SaaS minimalista y compacto, o refinar dashboards, CRM, herramientas internas,
  ajustes y aplicaciones de IA con esta dirección visual. Aplicar sobre el stack
  existente y adaptar a escritorio y móvil.
---

# Heyrico UI

Construir interfaces de trabajo con una jerarquía precisa y una estructura tranquila. Dar personalidad mediante proporciones, tipografía y detalles de interacción. Utilizar el color con intención.

Tratar esta skill como una interpretación independiente del estilo estudiado. Consultar [references/sources.md](references/sources.md) para distinguir los valores publicados de las decisiones propuestas. No presentar estos recursos como un sistema oficial del diseñador.

## 1. Entender el producto y el proyecto

1. Leer las instrucciones del proyecto e inspeccionar el stack, componentes, estilos, rutas y comportamiento afectados.
2. Identificar la tarea principal de la pantalla, el contenido necesario y la acción que permite avanzar. Elegir la estructura a partir de esa tarea.
3. Reutilizar componentes, tokens e iconos del proyecto. Introducir cambios localizados; respetar la arquitectura y la identidad de marca que el usuario quiera conservar.
4. Resolver decisiones reversibles con criterio. Preguntar solo cuando falte una decisión que cambie sustancialmente el producto.
5. Conservar el tema existente. En una interfaz nueva sin preferencia, empezar por el tema claro; preparar el oscuro solo si forma parte del encargo o el producto ya ofrece ambos.

## 2. Establecer el lenguaje visual

### Tipografía

- Utilizar Inter si ya está disponible o la fuente de sistema. Utilizar SF Pro únicamente cuando esté disponible en el entorno; no descargarla ni incluirla como un recurso propio.
- Trabajar principalmente con pesos 400 y 500. Reservar 600 para énfasis puntual.
- Tomar 13 px como base compacta de escritorio y emplear la escala 12, 13, 14, 16, 18 y 24 px. Usar 12 px para metadatos secundarios; ampliar texto de lectura extensa y formularios cuando lo necesiten.
- Aplicar -0.15 px de espaciado entre letras con Inter como punto de partida. Revisar su efecto en la fuente elegida.
- Construir jerarquía con tono, posición y espacio además de tamaño. Mantener títulos proporcionados a una herramienta de trabajo.
- Reservar la monoespaciada para código, identificadores o metadatos técnicos. Utilizar cifras tabulares en columnas numéricas.

### Superficies y color

- Construir la estructura con grises neutros y diferencias pequeñas de luminosidad entre fondo, panel y selección.
- En claro, partir de blanco, texto principal #292929 y texto secundario #5D5D5D. Utilizar #F5F5F5 en selecciones neutras y #F2F2F2 en separadores decorativos.
- En oscuro, partir de #1C1C1C, texto principal #E5E5E5 y secundario #A1A1A1. Diferenciar paneles mediante grises próximos.
- Utilizar bordes suaves en divisiones internas; aumentar el contraste cuando el borde sea necesario para reconocer un control.
- Conservar el color de marca existente. En un producto nuevo, utilizar un solo acento funcional y colores de estado coherentes.
- Concentrar saturación en series de datos, estados, identidades e ilustraciones localizadas. Mantener legibles las categorías sin depender solo del color.
- Dar profundidad ligera a paneles. Reservar sombras mayores para elementos flotantes. Evitar acumulación de sombras, bordes y fondos distintos en cada nivel.

### Geometría y ritmo

- Emplear radios de 6 y 8 px en controles; 12 px en paneles. Permitir 16 px en contenedores mayores cuando las proporciones lo pidan. Reservar las píldoras para etiquetas y acciones que encajen en ese tratamiento.
- Utilizar una escala de espacio corta y consistente: 4, 8, 12, 16, 24, 32 y 48 px como propuesta inicial.
- Dar más separación entre grupos que entre elementos del mismo grupo.
- Alinear ópticamente iconos, texto, contadores y acciones. Mantener consistentes la altura de fila y los márgenes internos entre componentes equivalentes.

Antes de escribir estilos, leer [assets/tokens.css](assets/tokens.css). Incorporar solo los valores necesarios al sistema existente y utilizar sus convenciones de nombres. Tratar el CSS como una base adaptable, no como una hoja global que deba imponerse al proyecto.

## 3. Componer según el contenido

- Utilizar sidebar, cabecera y contenido principal cuando exista navegación entre áreas. Omitir la sidebar en flujos autocontenidos.
- Empezar con una sidebar de 232 px y controles de unos 32 px en escritorio como propuestas ajustables. Priorizar la legibilidad de etiquetas reales.
- Agrupar la navegación con encabezados pequeños. Mantener iconos finos, espacios regulares y selección mediante un fondo discretamente distinto.
- Construir cabeceras compactas: título, contexto y acciones relacionadas. Dar máxima prioridad a una acción por área de trabajo.
- Elegir tablas para comparar registros, listas para contenido secuencial y paneles para grupos que tengan identidad propia. Utilizar tarjetas cuando su agrupación ayude a la tarea.
- Mantener regiones continuas para tablas y listas. Evitar convertir cada fila o frase en una tarjeta independiente.
- Mostrar contenido y ejemplos realistas, acordes al producto y al idioma del usuario.

Leer [references/components.md](references/components.md) antes de implementar tablas, menús, formularios, paneles de IA o gráficas. Elegir los patrones pertinentes; no añadir componentes solo para demostrar el estilo.

## 4. Implementar comportamiento y adaptación

- Reutilizar la familia de iconos instalada. En un proyecto nuevo, elegir una sola familia con trazos finos; tomar Nucleo o Hugeicons como referencias visuales. No añadir varias bibliotecas para conseguir iconos concretos.
- Utilizar elementos semánticos y controles de la biblioteca existente. Conectar acciones a comportamiento real o a un estado local coherente si se solicita un prototipo.
- Mostrar selección, foco, hover, pulsación y deshabilitado de forma distinguible. Añadir estados de carga, vacío y error donde el flujo pueda producirlos.
- Mantener etiquetas visibles en formularios, nombres accesibles en botones de icono, navegación de teclado y foco visible. Hacer operables y cerrables menús y diálogos con el teclado.
- Comprobar contraste suficiente: tomar 4.5:1 para texto normal y 3:1 para controles e indicadores esenciales como objetivos mínimos. Ajustar los grises de referencia cuando fallen. No aplicar los separadores casi blancos a indicadores esenciales.
- En táctil, ampliar las áreas de interacción y usar texto de formulario de al menos 16 px cuando sea necesario para evitar el zoom de iOS. Separar el tamaño visual del tamaño del área pulsable.
- Adaptar según el contenido: colapsar la sidebar en pantallas estrechas, reorganizar barras de herramientas y mantener accesibles las acciones principales. Para tablas anchas, permitir desplazamiento dentro de su región o una vista alternativa que conserve las comparaciones importantes.
- Asegurar que el documento no desborde horizontalmente. Mantener los popovers dentro de la ventana y definir desplazamiento interno para menús largos.
- Si hace falta movimiento, utilizar transiciones breves de color, opacidad o posición; respetar movimiento reducido. Tratar la animación como una decisión de implementación, no como una propiedad comprobada en capturas estáticas.

## 5. Revisar el resultado visible

1. Ejecutar las comprobaciones existentes que correspondan a los cambios. Evitar añadir infraestructura de pruebas para una modificación visual pequeña.
2. Renderizar la pantalla e inspeccionarla con las herramientas de vista previa o navegador disponibles. Revisar una anchura de escritorio y una estrecha; añadir otras si aparece un problema. No afirmar una revisión visual a partir de leer el CSS.
3. Comprobar un flujo principal y los estados relevantes de los componentes cambiados.
4. Corregir los defectos observados y volver a comprobar las zonas afectadas.
5. Informar brevemente de lo implementado, de la verificación realizada y de cualquier limitación real.

Aplicar esta revisión concreta antes de entregar:

- ¿Se reconoce la tarea principal y se encuentra su acción?
- ¿La tipografía y los controles conservan la densidad compacta sin dificultar la lectura?
- ¿Los encabezados, metadatos y acciones tienen niveles de énfasis distintos?
- ¿Coinciden las alineaciones, radios, alturas y separaciones entre componentes equivalentes?
- ¿El color comunica significado y las superficies siguen siendo tranquilas?
- ¿Se distinguen foco, selección, estados y controles con contraste suficiente?
- ¿Funcionan filtros, menús, pestañas y acciones que se presentan como operativos?
- ¿Se puede utilizar la pantalla con teclado y en una ventana estrecha?
- ¿El contenido largo, vacío o cargando mantiene la estructura?
- ¿Los fondos decorativos de las referencias se han interpretado como presentación y no como parte obligatoria del producto?

Si no se puede renderizar o interactuar en este entorno, realizar la revisión estática disponible e indicar con precisión qué queda sin verificar. No dar por superada una comprobación que no se haya ejecutado.
