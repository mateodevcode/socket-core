// Autorización del backend de ejemplo, explícita y testeable.
// En una app real aquí va TU política (rol, tenant, ACL, membresía del canal…).
// En el ejemplo: hace falta sesión válida y coincidir con el canal configurado.
export function canAccess(user, channel, allowedChannel) {
  return Boolean(user && user.id) && channel === allowedChannel;
}
