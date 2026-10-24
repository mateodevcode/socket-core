// Presencia local: cuenta conexiones por usuario (Set de connection_id).
// Necesario porque la renovación del JWT abre una conexión nueva antes de cerrar
// la vieja: un client_left de la vieja NO debe marcar offline al usuario.
export function createPresenceTracker() {
  const porUsuario = new Map(); // user -> Set(connection_id)
  const setDe = (user) => {
    if (!porUsuario.has(user)) porUsuario.set(user, new Set());
    return porUsuario.get(user);
  };
  return {
    /** Siembra el estado con la foto inicial (GET /presence del backend). */
    seed(list) {
      for (const c of list || []) {
        if (c && c.user && c.connection_id) setDe(c.user).add(c.connection_id);
      }
    },
    /** Aplica client_joined/client_left y devuelve el estado del usuario. */
    apply(ev) {
      if (!ev || !ev.user) return { user: null, online: false, connections: 0 };
      const set = setDe(ev.user);
      if (ev.type === "client_joined" && ev.connection_id) set.add(ev.connection_id);
      else if (ev.type === "client_left" && ev.connection_id) set.delete(ev.connection_id);
      const connections = set.size;
      if (connections === 0) porUsuario.delete(ev.user);
      return { user: ev.user, online: connections > 0, connections };
    },
    count(user) {
      return porUsuario.get(user)?.size || 0;
    },
    online() {
      return [...porUsuario.keys()];
    },
  };
}
