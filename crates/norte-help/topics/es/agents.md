+++
id = "agents"
title = "Cuando pregunta un agente"
tags = ["agents"]
see_also = ["dialogs", "copying", "settings"]
commands = ["dialog.approve", "dialog.deny"]
context = ["dialog.approval"]
+++
Un agente de IA puede pilotar norte —listar, leer, copiar, mover, borrar— a
través de un puente que corre como un proceso aparte y habla el mismo protocolo
que tu propia sesión. No tiene sistema de ficheros propio: cada petición que
hace llega aquí, bajo la política que tú fijaste, y las que te necesitan
producen el diálogo desde el que probablemente estés leyendo esto.

{{cmd:dialog.approve}} deja pasar esa petición. {{cmd:dialog.deny}} la rechaza.
Cerrar el diálogo es DENEGAR, y marcharse también: nada se aprueba por
agotarse el tiempo.

La petición dice quién pregunta, qué quiere hacer y qué rutas tocaría: una ruta
por línea, cada una etiquetada, jamás cosidas dentro de una frase.

> ⚠ Lee las rutas, no la frase que las rodea. Un nombre se puede fabricar para que se lea como otro: bytes distintos, idénticos en pantalla. Lo que se enseña aquí va enmascarado y MARCADO cuando se ha alterado, y esa marca es la señal de que un nombre no es lo que parece.

# Scopes: la respuesta que se da una vez

Aprobar cada petición de una en una cansa, así que un agente puede pedir un
**scope**: un subárbol, un conjunto de operaciones y un plazo. Concederlo es
decisión tuya y solo tuya —un agente no puede concederse nada— y la petición te
llega igual.

Dentro de su scope el agente trabaja sin preguntar. Fuera, la respuesta es no:
no una pregunta, un rechazo. Esa es la dirección que mantiene esto en pie —
una regla que falta deniega, en vez de caer hacia el sí.

Un scope caduca. Cuando lo hace, el agente vuelve a preguntar, y no tiene forma
de renovárselo él.

# Nada de lo que hace un agente es invisible

Toda mutación pasa por el diario antes de confirmarse, etiquetada con quién la
hizo: tú, un agente (con su sesión) o un plugin. Ese registro es lo que hace
posible lo siguiente.

**Puedes deshacer la sesión de un agente, y no necesitas su permiso.** El
deshacer corre como TÚ, así que funciona aunque el scope del agente haya
caducado y aunque el agente ya no esté. Va hacia atrás, de lo más reciente a lo
más antiguo, porque deshacer una secuencia en desorden es la forma de acabar en
un estado que no pidió nadie.

Dos desenlaces no son fallos y se reportan en vez de taparse. Un paso que nunca
fue reversible —algo borrado definitivamente— se SALTA y se cuenta, así que el
resto de la sesión sí vuelve. Un paso que la política ahora bloquea DETIENE el
deshacer donde está, y el informe nombra ese paso: seguir más allá dejaría un
árbol a medio deshacer sin nada que diga por dónde está la costura.

Borrar a la papelera es lo que hace reversible casi todo, para empezar. De eso
va [[copying]].

> 💡 Si las funciones de IA no son algo que quieras, se apagan en los ajustes: eso es un ajuste, no una decisión de política, y está en [[settings]].
