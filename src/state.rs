use failure::{format_err, Error};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StateEvent {
    Request,
    InfoResponse,
    Response,
    Data,
    EndOfMessage,
    ConnectionClosed,
}

#[derive(Clone, Copy, Debug)]
pub enum SwitchEvent {
    Connect,
    Upgrade,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Client {
    Idle,
    SendBody,
    Done,
    MustClose,
    Closed,
    MightSwitchProtocol,
    SwitchedProtocol,
    Error,
}

impl Client {
    fn send(self, event: StateEvent) -> Result<Self, Error> {
        use self::Client::*;
        use self::StateEvent::*;

        Ok(match (self, event) {
            (Idle, Request) | (SendBody, Data) => SendBody,
            (SendBody, EndOfMessage) => Done,
            (Idle, ConnectionClosed)
            | (Done, ConnectionClosed)
            | (MustClose, ConnectionClosed)
            | (Closed, ConnectionClosed) => Closed,
            _ => return Err(format_err!("invalid state transition")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Server {
    Idle,
    SendResponse,
    SendBody,
    Done,
    MustClose,
    Closed,
    SwitchedProtocol,
    Error,
}

impl Server {
    fn send(
        self,
        event: StateEvent,
        switch: Option<SwitchEvent>,
    ) -> Result<Self, Error> {
        use self::Server::*;
        use self::StateEvent::*;
        use self::SwitchEvent::*;

        Ok(match (self, event, switch) {
            (Idle, Request, None) | (SendResponse, InfoResponse, None) => {
                SendResponse
            }
            (SendResponse, InfoResponse, Some(Upgrade))
            | (SendResponse, Response, Some(Connect)) => SwitchedProtocol,
            (Idle, Response, None)
            | (SendResponse, Response, None)
            | (SendBody, Data, None) => SendBody,
            (SendBody, EndOfMessage, None) => Done,
            (Idle, ConnectionClosed, None)
            | (Done, ConnectionClosed, None)
            | (MustClose, ConnectionClosed, None)
            | (Closed, ConnectionClosed, None) => Closed,
            _ => return Err(format_err!("invalid state transition")),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct State {
    client: Client,
    server: Server,
    keep_alive: bool,
    pub pending_connect: bool,
    pending_upgrade: bool,
}

impl State {
    pub fn new() -> Self {
        Self {
            client: Client::Idle,
            server: Server::Idle,
            keep_alive: true,
            pending_connect: false,
            pending_upgrade: false,
        }
    }

    fn states(self) -> (Client, Server) {
        (self.client, self.server)
    }

    pub fn client_event(self, event: StateEvent) -> Result<Self, Error> {
        Ok(Self {
            client: self.client.send(event)?,
            server: if event == StateEvent::Request {
                self.server.send(StateEvent::Request, None)?
            } else {
                self.server
            },
            ..self
        }
        .state_transitions())
    }

    pub fn server_event(
        self,
        event: StateEvent,
        switch: Option<SwitchEvent>,
    ) -> Result<Self, Error> {
        match switch {
            Some(SwitchEvent::Connect) if !self.pending_connect => {
                return Err(format_err!("cannot connect without proposal"));
            }
            Some(SwitchEvent::Upgrade) if !self.pending_upgrade => {
                return Err(format_err!("cannot upgrade without proposal"));
            }
            _ => {}
        }
        Ok(Self {
            server: self.server.send(event, switch)?,
            pending_connect: if switch.is_none()
                && event == StateEvent::Response
            {
                false
            } else {
                self.pending_connect
            },
            pending_upgrade: if switch.is_none()
                && event == StateEvent::Response
            {
                false
            } else {
                self.pending_upgrade
            },
            ..self
        }
        .state_transitions())
    }

    pub fn client_error(self) -> Self {
        Self {
            client: Client::Error,
            ..self
        }
        .state_transitions()
    }

    pub fn server_error(self) -> Self {
        Self {
            server: Server::Error,
            ..self
        }
        .state_transitions()
    }

    pub fn connect_proposal(self) -> Self {
        Self {
            pending_connect: true,
            ..self
        }
        .state_transitions()
    }

    pub fn upgrade_proposal(self) -> Self {
        Self {
            pending_upgrade: true,
            ..self
        }
        .state_transitions()
    }

    pub fn disable_keep_alive(self) -> Self {
        Self {
            keep_alive: false,
            ..self
        }
        .state_transitions()
    }

    pub fn start_next_cycle(self) -> Result<Self, Error> {
        if (self.client, self.server) != (Client::Done, Server::Done) {
            return Err(format_err!("not in reusable state"));
        }
        Ok(Self {
            client: Client::Idle,
            server: Server::Idle,
            ..self
        })
    }

    fn state_transitions(mut self) -> Self {
        loop {
            let start_states = self.states();

            if self.any_pending() && self.client == Client::Done {
                self.client = Client::MightSwitchProtocol;
            }

            if !self.any_pending()
                && self.client == Client::MightSwitchProtocol
            {
                self.client = Client::Done;
            }

            if !self.keep_alive {
                if self.client == Client::Done {
                    self.client = Client::MustClose;
                }
                if self.server == Server::Done {
                    self.server = Server::MustClose;
                }
            }

            match (self.client, self.server) {
                (Client::MightSwitchProtocol, Server::SwitchedProtocol) => {
                    self.client = Client::SwitchedProtocol;
                }
                (Client::Closed, Server::Done)
                | (Client::Closed, Server::Idle)
                | (Client::Error, Server::Done) => {
                    self.server = Server::MustClose;
                }
                (Client::Done, Server::Closed)
                | (Client::Idle, Server::Closed)
                | (Client::Done, Server::Error) => {
                    self.client = Client::MustClose;
                }
                _ => {}
            }

            if start_states == self.states() {
                return self;
            }
        }
    }

    fn any_pending(self) -> bool {
        self.pending_connect || self.pending_upgrade
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use self::StateEvent::*;
    use self::SwitchEvent::*;

    #[test]
    fn basic_transitions() {
        let mut cs = State::new();

        assert_eq!((Client::Idle, Server::Idle), cs.states());

        cs = cs.client_event(Request).expect("client sends request");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        assert!(cs.client_event(Request).is_err());

        cs = cs
            .server_event(InfoResponse, None)
            .expect("server sends info response");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert_eq!((Client::SendBody, Server::SendBody), cs.states());

        cs = cs
            .client_event(EndOfMessage)
            .expect("client ends message")
            .server_event(EndOfMessage, None)
            .expect("server ends message");
        assert_eq!((Client::Done, Server::Done), cs.states());

        cs = cs
            .server_event(ConnectionClosed, None)
            .expect("server closes connection");
        assert_eq!((Client::MustClose, Server::Closed), cs.states());
    }

    #[test]
    fn disable_keep_alive() {
        let mut cs = State::new()
            .client_event(Request)
            .expect("client sends request")
            .disable_keep_alive()
            .client_event(EndOfMessage)
            .expect("client ends message");
        assert_eq!((Client::MustClose, Server::SendResponse), cs.states());

        cs = cs
            .server_event(Response, None)
            .expect("server sends response")
            .server_event(EndOfMessage, None)
            .expect("server ends message");
        assert_eq!((Client::MustClose, Server::MustClose), cs.states());
    }

    #[test]
    fn disable_keep_alive_in_done() {
        let mut cs = State::new()
            .client_event(Request)
            .expect("client sends request")
            .client_event(EndOfMessage)
            .expect("client ends message");
        assert_eq!(Client::Done, cs.client);
        cs = cs.disable_keep_alive();
        assert_eq!(Client::MustClose, cs.client);
    }

    #[test]
    fn connect_switch_denied_early() {
        let mut cs = State::new()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert!(!cs.pending_connect);

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!((Client::Done, Server::SendBody), cs.states());
    }

    #[test]
    fn connect_switch_denied_late() {
        let mut cs = State::new()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );
        cs = cs
            .server_event(InfoResponse, None)
            .expect("server sends info response");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert_eq!((Client::Done, Server::SendBody), cs.states());
        assert!(!cs.pending_connect);
    }

    #[test]
    fn upgrade_switch_denied_early() {
        let mut cs = State::new()
            .upgrade_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert!(!cs.pending_upgrade);

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!((Client::Done, Server::SendBody), cs.states());
    }

    #[test]
    fn upgrade_switch_denied_late() {
        let mut cs = State::new()
            .upgrade_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );
        cs = cs
            .server_event(InfoResponse, None)
            .expect("server sends info response");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert_eq!((Client::Done, Server::SendBody), cs.states());
        assert!(!cs.pending_upgrade);
    }

    #[test]
    fn connect_switch_accepted() {
        let mut cs = State::new()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(InfoResponse, None)
            .expect("server sends info response");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(Response, Some(Connect))
            .expect("server sends response accepting connect");
        assert_eq!(
            (Client::SwitchedProtocol, Server::SwitchedProtocol),
            cs.states()
        );
    }

    #[test]
    fn upgarde_switch_accepted() {
        let mut cs = State::new()
            .upgrade_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(InfoResponse, None)
            .expect("server sends info response");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(InfoResponse, Some(Upgrade))
            .expect("server sends info response accepting upgrade");
        assert_eq!(
            (Client::SwitchedProtocol, Server::SwitchedProtocol),
            cs.states()
        );
    }

    #[test]
    fn double_protocol_switch_deny() {
        let mut cs = State::new()
            .upgrade_proposal()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data")
            .client_event(EndOfMessage)
            .expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );
        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert_eq!((Client::Done, Server::SendBody), cs.states());
    }

    #[test]
    fn double_protocol_switch_connect() {
        let mut cs = State::new()
            .upgrade_proposal()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data")
            .client_event(EndOfMessage)
            .expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );
        cs = cs
            .server_event(Response, Some(Connect))
            .expect("server sends response accepting connect");
        assert_eq!(
            (Client::SwitchedProtocol, Server::SwitchedProtocol),
            cs.states()
        );
    }

    #[test]
    fn double_protocol_switch_upgrade() {
        let mut cs = State::new()
            .upgrade_proposal()
            .connect_proposal()
            .client_event(Request)
            .expect("client sends request")
            .client_event(Data)
            .expect("client sends data")
            .client_event(EndOfMessage)
            .expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );
        cs = cs
            .server_event(InfoResponse, Some(Upgrade))
            .expect("server sends info response accepting upgrade");
        assert_eq!(
            (Client::SwitchedProtocol, Server::SwitchedProtocol),
            cs.states()
        );
    }

    #[test]
    fn bad_protocol_switch_server_connect() {
        let cs = State::new()
            .client_event(Request)
            .expect("client sends request");
        assert!(cs.server_event(Response, Some(Connect)).is_err());
    }

    #[test]
    fn bad_protocol_switch_server_upgrade() {
        let cs = State::new()
            .client_event(Request)
            .expect("client sends request");
        assert!(cs.server_event(Response, Some(Upgrade)).is_err());
    }

    #[test]
    fn bad_protocol_switch_client_upgrade_server_connect() {
        let cs = State::new()
            .client_event(Request)
            .expect("client sends request")
            .upgrade_proposal();
        assert!(cs.server_event(Response, Some(Connect)).is_err());
    }

    #[test]
    fn bad_protocol_switch_client_connect_server_upgrade() {
        let cs = State::new()
            .client_event(Request)
            .expect("client sends request")
            .connect_proposal();
        assert!(cs.server_event(Response, Some(Upgrade)).is_err());
    }

    #[test]
    fn keep_alive_protocol_switch() {
        let mut cs = State::new()
            .upgrade_proposal()
            .client_event(Request)
            .expect("client sends request")
            .disable_keep_alive()
            .client_event(Data)
            .expect("client sends data");
        assert_eq!((Client::SendBody, Server::SendResponse), cs.states());

        cs = cs.client_event(EndOfMessage).expect("client ends message");
        assert_eq!(
            (Client::MightSwitchProtocol, Server::SendResponse),
            cs.states()
        );

        cs = cs
            .server_event(Response, None)
            .expect("server sends response");
        assert_eq!((Client::MustClose, Server::SendBody), cs.states());
    }

    #[test]
    fn connection_reuse() {
        let mut cs = State::new();

        assert!(cs.start_next_cycle().is_err());

        cs = cs
            .client_event(Request)
            .expect("client sends request")
            .client_event(EndOfMessage)
            .expect("client ends message");

        assert!(cs.start_next_cycle().is_err());

        cs = cs
            .server_event(Response, None)
            .expect("server sends response")
            .server_event(EndOfMessage, None)
            .expect("server ends message");

        cs = cs.start_next_cycle().expect("start next cycle");
        assert_eq!((Client::Idle, Server::Idle), cs.states());

        cs = cs
            .client_event(Request)
            .expect("client sends request")
            .disable_keep_alive()
            .client_event(EndOfMessage)
            .expect("client ends message")
            .server_event(Response, None)
            .expect("server sends response")
            .server_event(EndOfMessage, None)
            .expect("server ends message");

        assert!(cs.start_next_cycle().is_err());

        let cs = State::new()
            .client_event(Request)
            .expect("client sends request")
            .disable_keep_alive()
            .client_event(EndOfMessage)
            .expect("client ends message")
            .client_event(ConnectionClosed)
            .expect("client closes connection")
            .server_event(Response, None)
            .expect("server sends response")
            .server_event(EndOfMessage, None)
            .expect("server ends message");

        assert!(cs.start_next_cycle().is_err());
    }
}
