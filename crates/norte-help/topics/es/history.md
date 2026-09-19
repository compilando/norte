+++
id = "history"
title = "Historia de navegación"
tags = ["basics"]
see_also = ["panes", "tabs"]
commands = [
    "nav.back",
    "nav.forward",
    "pane.history",
    "pane.history-left",
    "pane.history-right",
    "dialog.confirm",
    "dialog.confirm-other",
    "dialog.remove",
    "dialog.clear",
    "dialog.add",
    "dialog.filter",
    "pane.popular",
    "nav.set-jump-point",
    "nav.jump-back",
    "pane.hotlist",
    "app.goto",
]
+++

# Historia de navegación

Cada panel recuerda por dónde ha pasado, y lo recuerda de tres formas
porque son tres preguntas distintas: de dónde vengo, por dónde he estado y a
dónde voy siempre.

## Atrás y adelante

{{cmd:nav.back}} devuelve el panel al directorio del que venía, y
{{cmd:nav.forward}} deshace ese paso. Cada panel recorre su propio rastro, y
ninguna de las dos teclas mueve el foco.

Es un RASTRO, no una lista. De un directorio a un segundo y de ahí a un
tercero, dos veces atrás llega al primero. Una lista de los últimos visitados,
recorrida como si fuera un rastro, oscilaría entre los dos más recientes para
siempre; por eso «dónde estaba hace un momento» y «por dónde ha pasado este
panel» son dos preguntas distintas: la segunda es el popup de
{{cmd:pane.history}}, y volver atrás nunca le añade nada.

Navegar a un sitio nuevo desde la mitad del rastro olvida la rama de la que te
saliste, igual que en un navegador. Ofrecer un «adelante» hacia una historia
que ya has abandonado es el fallo que todo el mundo conoce.

Un paso que no llega se rebobina: no te fuiste, así que el rastro se queda como
estaba. Vale tanto para el paso que **falla** como para el que **abandonas**
con Esc mientras lista: en los dos casos el panel sigue enseñando lo mismo, y
un rastro que diera el paso por bueno te mandaría «adelante» al directorio que
ya está en pantalla. Cuando el motivo es que el directorio **ya no está**,
además sale del rastro, de la rama de delante y del popup de historial, de modo
que la tecla no puede dejarte atrapado en un directorio que se ha demostrado
que no existe. Cualquier otro fallo lo conserva: un host caído o un directorio
que no puedes leer siguen siendo sitios, y pueden responder al siguiente
intento.

El paso que se para a preguntar por la clave desconocida de un host es el único
que ESPERA: ni se da ni se deshace hasta que respondes, porque confiar en la
clave reanuda esa misma navegación. Si confías, el paso se termina; si deniegas,
o si el paso reanudado falla, se rebobina como cualquier otro que no llegó.

> 💡 Cuando ya no queda rastro hacia atrás, la tecla lo dice. Una tecla que se calla es indistinguible de una rota.

## La lista

{{cmd:pane.history}} abre la lista de los sitios por los que ha pasado el
panel, del más reciente al más viejo. La primera fila es donde estás ahora,
marcada «aquí», y el cursor empieza en la siguiente. Las que todavía puedes
recuperar con {{cmd:nav.forward}} llevan «adelante».

{{cmd:pane.history-left}} y {{cmd:pane.history-right}} abren la de un lado
concreto de la pantalla: lo que elijas navega ese panel aunque el foco esté
en el otro.

Dentro de la lista:

- {{cmd:dialog.confirm}} va al directorio;
- {{cmd:dialog.confirm-other}} lo abre en el OTRO panel, sin mover el foco;
- {{cmd:dialog.remove}} lo quita de la lista y del rastro;
- {{cmd:dialog.clear}} vacía la historia de ese panel;
- {{cmd:dialog.add}} guarda la fila como favorito, con el nombre ya propuesto;
- {{cmd:dialog.filter}} filtra la lista mientras tecleas, y `Esc` quita el
  filtro.

## Populares

{{cmd:pane.popular}} lista los directorios a los que más vas, ordenados por
visitas. Es UNA lista para toda la sesión, no una por panel: la pregunta es a
dónde sueles ir, y no cambia según el lado desde el que la hagas. Las mismas
teclas de la lista valen aquí.

## Punto de salto

{{cmd:nav.set-jump-point}} marca el directorio actual del panel y
{{cmd:nav.jump-back}} vuelve a él desde donde estés. Volver es una
navegación normal, así que {{cmd:nav.back}} te devuelve a donde estabas antes
del salto.

## Qué se guarda

La historia, el punto de salto y los populares viven en la sesión: siguen ahí
al volver a abrir norte. Cuántos directorios recuerda cada panel lo decide
`[ui] history_size`, entre 5 y 64; si no dices nada, 30.

Los favoritos ({{cmd:pane.hotlist}}) son otra cosa: los eliges tú y viven en
tu configuración.

> 💡 Un directorio al que vuelves a menudo merece un favorito: el panel recuerda por dónde ha pasado, y los favoritos son comunes a los dos paneles.

En la ventana, los botones laterales del ratón son atrás y adelante. Un
terminal no recibe esos botones.

# Ir a cualquier sitio

{{cmd:app.goto}} abre una sola pantalla con todo lo que puede ser un
destino, en secciones: la ruta que estás tecleando, la historia de este
panel, los sitios a los que más vuelves, tus favoritos, tus conexiones, los
comandos, y —si tienes índice semántico— lo que el índice encuentre. Escribe
y se va quedando lo que casa; las flechas se mueven fila a fila, saltándose
los títulos, e Intro te lleva.

No sustituye a ninguna de las listas de arriba: cada una sigue teniendo su
tecla y su pantalla, donde se ven enteras y se pueden borrar entradas. Ésta
es la que sirve cuando no te acuerdas de en cuál de las cinco estaba lo que
buscas.

Tres cosas que conviene saber. Una ruta cuenta como ruta si empieza por `/`,
por `~` o si lleva esquema (`sftp://…`); `~` es tu carpeta personal, no la
del panel. Una relativa no cuenta a propósito: «a dónde voy» no puede
depender de dónde estabas. Y el índice semántico se pregunta a partir de
tres letras, contesta cuando puede —no es instantáneo— y su sección aparece
abajo sin mover lo que estuvieras mirando; si no lo tienes encendido,
simplemente no sale.

En los presets `orthodox`, `cua` y `vim` está en `ctrl+g`. Los cuatro
importados no lo atan, porque ninguno de los gestores que transcriben tiene
una tecla equivalente: en ésos se llega por el menú Ir, donde está el
primero.
