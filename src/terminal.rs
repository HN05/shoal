//! The terminal hosts Shoal runs work in: the [`Host`] trait, the
//! capability traits in [`host`] a host may support, the registry that
//! chooses one under config, and the hosts themselves. Generic code never
//! names a host; adding one means naming it in
//! [`HostName`](crate::sessions::HostName), implementing [`Host`] and the
//! capability traits it supports, and registering it where the compiler then
//! asks; a `default_host` value of its name makes it the host for terminals
//! no host serves.
mod herdr;
mod host;
mod hosts;
mod rex;
mod sessions;
mod tmux;
mod typed;
mod zellij;
mod zmx;

#[cfg(test)]
pub(crate) use host::Closes;
pub(crate) use host::{
    Attaches, Demo, DemoWorker, Host, NOT_INHERITED_BY_DEMOS, PlaceState, Places, Sessions, Start,
    Watched, Watches,
};
pub(crate) use hosts::{
    Beside, Configured, Options, background_place, current, enabled_host, hosts, installed_hosts,
    named, names_places,
};
pub(crate) use sessions::{Owner, OwnerKind, Session};
pub(crate) use typed::run as run_launch;
