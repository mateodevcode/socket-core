# Guía del operador

Cómo dar de alta, ver, modificar y retirar proyectos en socket-core.
Todo se hace con una sola herramienta: `scripts/socketcore.py`.

> Esta guía no contiene credenciales ni datos de ningún despliegue. Los
> nombres (`mi-proyecto`, etc.) son de ejemplo.

## 1. Qué es un "proyecto"

Cada aplicación que usa el hub es un **proyecto**. Al crearlo se le entrega una
**API key** y el hub le asigna un espacio propio:

| Dato | Para qué sirve |
|---|---|
| **Nombre** | Identifica al proyecto (único) |
| **Prefijo de canales** (`mi-proyecto:*`) | Solo puede crear y usar canales cuyo nombre empiece así. Dos proyectos no pueden solaparse |
| **Scopes** | Qué puede hacer: `publish` (emitir), `subscribe` (escuchar), `manage` (crear y borrar canales) |
| **Dominios** | Orígenes web desde los que se pueden pedir tokens. Solo frena a navegadores ajenos; **no** sustituye a la API key |
| **Límite** | Peticiones por minuto de esa key |
| **Caducidad** | Opcional |

Las tres credenciales del sistema, de mayor a menor poder:

1. **`MASTER_KEY`**: la del operador. Crea, edita, revoca y borra proyectos. Vive solo en el `.env` del servidor.
2. **API key del proyecto**: la tiene el *backend* de cada app (nunca el navegador). Con ella pide tokens y usa el REST.
3. **JWT**: pase de 10 minutos para un usuario concreto. Lo pide el backend y lo usa el navegador.

## 2. Requisitos

- Ejecutarlo **en el servidor** donde corre el contenedor `socket-core`.
- El contenedor `socket-core` en marcha, y tu usuario con permiso para usar Docker.
- Un `.env` legible con `MASTER_KEY=...` en la carpeta del proyecto.
- Python 3 (solo librería estándar, no hay nada que instalar).

La herramienta se ejecuta siempre desde la carpeta del proyecto:

```bash
cd /ruta/al/proyecto
python3 scripts/socketcore.py --help
```

## 3. Comandos

| Quiero... | Comando |
|---|---|
| Ver todos los proyectos | `python3 scripts/socketcore.py listar` |
| Crear un proyecto | `python3 scripts/socketcore.py alta` |
| Revocar | `python3 scripts/socketcore.py revocar NOMBRE` |
| Borrar | `python3 scripts/socketcore.py borrar NOMBRE` |
| Cambiar algo | `python3 scripts/socketcore.py editar NOMBRE --rate 300` |

Los comandos que modifican algo **piden confirmación**. `listar` solo lee.

### `listar`

Una ficha por proyecto:

```
● mi-proyecto   [activa]   id=…
    prefijo: mi-proyecto:*   scopes: publish,subscribe   límite: 120/min
    dominios: https://app.ejemplo.com   expira: nunca
    canales: 4   conexiones ahora: 2
    paquete de credenciales: /home/usuario/credenciales-socketcore/credenciales-mi-proyecto-….txt
```

Si dice `(no hay fichero…)`, la key solo se mostró al crearla y ya no existe en el servidor.

### `alta`

Abre el asistente (`scripts/alta_cliente.py`), que pregunta nombre, dominios,
prefijo, scopes, límite y vigencia, y escribe un **paquete de credenciales**
con permisos 0600 en `~/credenciales-socketcore/`. La API key **no se imprime
en pantalla**. Necesita `SOCKET_PUBLIC_URL` (o `--core`) con la URL pública del hub.

Recomendaciones al dar de alta:
- Scopes por defecto `publish,subscribe`. Añade `manage` solo si la app crea canales.
- Dominios reales, no `*`.
- Vigencia corta para pruebas.

### `revocar NOMBRE`

La key deja de valer, **las conexiones abiertas se cortan al instante** y los
tokens ya emitidos dejan de servir. No se puede deshacer, y **el prefijo sigue
ocupado** hasta que se borre el proyecto.

### `borrar NOMBRE`

Elimina el proyecto **y todos sus canales**, y libera el prefijo. Hay que
escribir el nombre para confirmar. Los mensajes no se guardan en el hub, así
que lo único que se pierde son los canales y la presencia.

### `editar NOMBRE`

Cambia solo lo que indiques:

| Opción | Ejemplo |
|---|---|
| `--scopes` | `--scopes publish,subscribe` |
| `--dominios` | `--dominios https://app.ejemplo.com` |
| `--rate` | `--rate 300` |
| `--expira-dias` | `--expira-dias 30` (`0` quita la caducidad) |
| `--prefijo` | `--prefijo nuevo:*` |

Cambiar **scopes** o **prefijo** corta las conexiones abiertas del proyecto;
reconectan con los permisos nuevos.

## 4. Entregar las credenciales a una app

1. Crea el proyecto con `alta`.
2. Copia la `API_KEY` del fichero `~/credenciales-socketcore/credenciales-NOMBRE-….txt` a la
   configuración **secreta del servidor** de la app (variable de entorno del backend,
   gestor de secretos). Nunca al navegador, ni a un repositorio, ni a un chat.
3. **Borra el fichero** del servidor.
4. Comprueba con `listar` que el proyecto aparece activo.

Si la app pierde la key no hay forma de recuperarla: se crea otra.

## 5. Procedimientos

### Una key se ha filtrado

En este orden, sin esperar:

1. `revocar NOMBRE`: corta el daño al instante.
2. `borrar NOMBRE`: libera el prefijo.
3. `alta` con el mismo nombre y prefijo, y entrega la nueva key a la app.
4. La app vuelve a crear sus canales.

Hoy no existe la rotación en un solo paso que conserve los canales; está en
`docs/PENDIENTE.md`.

### Retirar un proyecto

`revocar` (para cortar el acceso ya) y después `borrar` (para liberar el prefijo).

### Dar más capacidad a un proyecto

`editar NOMBRE --rate 600`. Si el hub corre detrás de un proxy en el mismo
servidor, recuerda que también existe un límite por IP (`IP_RATE_PER_MIN`).

## 6. Seguridad de la herramienta

- Habla con el panel de administración, que **solo existe dentro del contenedor**
  (no está publicado en ningún puerto).
- La `MASTER_KEY` se lee de `.env` y se envía a `curl` por la entrada estándar:
  no aparece en `ps`, ni en el historial, ni en la pantalla.
- Nunca imprime una API key. Los tests lo comprueban
  (`scripts/test_socketcore.py`).
- Mantén el `.env` con permisos `600` y propietario el usuario que opera.

## 7. Problemas frecuentes

| Mensaje | Causa probable |
|---|---|
| `El admin respondió HTTP 0` | El contenedor no está en marcha, o tu usuario no puede usar Docker |
| `No puedo leer MASTER_KEY` | No estás en la carpeta del proyecto, o el `.env` no es legible por tu usuario |
| `No existe el proyecto 'x'` | Nombre mal escrito: míralo con `listar` |
| `HTTP 400 … solapa` (al dar de alta) | El prefijo choca con otro proyecto, **incluidos los revocados**. Bórralo o elige otro prefijo |
| `HTTP 409` | Ya existe un proyecto con ese nombre |

## 8. Qué no hace

- No puede mostrar una API key ya creada.
- No puede "desrevocar".
- No rota una key conservando canales (pendiente).
- No sustituye a `scripts/estado.py` (salud del servicio) ni a los logs de auditoría
  (`docker logs socket-core 2>&1 | grep audit`).