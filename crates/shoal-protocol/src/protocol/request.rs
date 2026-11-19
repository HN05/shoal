//! The request envelope and the method each request carries.
use super::{
    AgentMethod, AlertMethod, AppMethod, ApprovalMethod, AssignmentMethod, BranchMethod,
    ChatMethod, CiMethod, ContainerMethod, DaemonMethod, DemoMethod, ExecutionMethod, ExitRequest,
    FileMethod, HoldMethod, HubMethod, ItemMethod, LaunchMethod, OperationMethod, PortMethod,
    QuestionMethod, QueueMethod, Reattach, RecoveryMethod, RemovalMethod, RepositoryMethod,
    ResourceMethod, ReviewMethod, SecretMethod, SimulatorMethod, SubscriptionMethod, TriggerMethod,
    VERSION, WorkspaceMethod,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub method: Method,
    #[serde(default)]
    pub scope: Option<crate::scope_tokens::ScopeToken>,
}

impl Request {
    /// An unscoped request, which `shoal-client`'s connection gives its
    /// caller's scope.
    pub fn new(method: impl Into<Method>) -> Self {
        Self {
            protocol: VERSION,
            id: 1,
            method: method.into(),
            scope: None,
        }
    }
}

/// Every request; each decodes from its own spelling.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Method {
    /// Answered before the protocol check and authorization.
    Reattach(Reattachment),
    /// Answered before the protocol check.
    Exit(ExitRequest),
    /// Long-lived: a hub member's link, checked against the protocol after
    /// it is read, so the hub can tell a member of another release why it
    /// refuses it.
    HubLink(crate::hub::link::Opening),
    Operation(Operation),
}

/// A request that resumes a connection rather than asking a domain.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reattachment {
    /// Long-lived: an execution connection resumed after a daemon restart.
    Reattach(Reattach),
}

/// A request one of the daemon's domains answers, by its domain.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Operation {
    Daemon(DaemonMethod),
    Repository(RepositoryMethod),
    Workspace(WorkspaceMethod),
    Removal(RemovalMethod),
    Recovery(RecoveryMethod),
    Execution(ExecutionMethod),
    Assignment(AssignmentMethod),
    Agent(AgentMethod),
    Chat(ChatMethod),
    Launch(LaunchMethod),
    Queue(QueueMethod),
    Hold(HoldMethod),
    Demo(DemoMethod),
    File(FileMethod),
    Item(ItemMethod),
    Ci(CiMethod),
    Branch(BranchMethod),
    Alert(AlertMethod),
    Operations(OperationMethod),
    Question(QuestionMethod),
    Review(ReviewMethod),
    Approval(ApprovalMethod),
    Port(PortMethod),
    Secret(SecretMethod),
    Resource(ResourceMethod),
    Simulator(SimulatorMethod),
    Container(ContainerMethod),
    Subscription(SubscriptionMethod),
    Trigger(TriggerMethod),
    Hub(HubMethod),
    App(AppMethod),
}

impl Method {
    /// The request's protocol name, for timing lines.
    pub fn name(&self) -> String {
        name_of(serde_json::to_value(self))
    }

    /// Daemon administration, which must reach a daemon of any release.
    pub fn controls_daemon(&self) -> bool {
        match self {
            Self::Reattach(_) | Self::HubLink(_) => false,
            Self::Exit(_) => true,
            Self::Operation(operation) => operation.controls_daemon(),
        }
    }

    /// How long the request keeps its connection.
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Reattach(_) => Lifetime::Resumed,
            Self::Exit(_) => Lifetime::Reply,
            Self::HubLink(_) => Lifetime::Open,
            Self::Operation(operation) => operation.lifetime(),
        }
    }

    /// Executions, waits and streams, which keep the connection open after
    /// the request.
    pub fn holds_connection(&self) -> bool {
        self.lifetime() != Lifetime::Reply
    }

    /// Long-lived requests whose clients send them again when the daemon
    /// closes the connection unanswered, as across a restart.
    pub fn retries_when_closed(&self) -> bool {
        self.lifetime() == Lifetime::Resumed
    }
}

/// The protocol name of a request spelled as `spelled`.
pub fn name_of(spelled: serde_json::Result<serde_json::Value>) -> String {
    match spelled {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(fields)) => fields
            .into_iter()
            .next()
            .map(|(name, _)| name)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

impl Operation {
    /// Daemon administration, which must reach a daemon of any release.
    pub fn controls_daemon(&self) -> bool {
        matches!(self, Self::Daemon(_))
    }

    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Repository(method) => method.lifetime(),
            Self::Workspace(method) => method.lifetime(),
            Self::Execution(method) => method.lifetime(),
            Self::Agent(method) => method.lifetime(),
            Self::Launch(method) => method.lifetime(),
            Self::Queue(method) => method.lifetime(),
            Self::Item(method) => method.lifetime(),
            Self::Alert(method) => method.lifetime(),
            Self::Operations(method) => method.lifetime(),
            Self::Question(method) => method.lifetime(),
            Self::Review(method) => method.lifetime(),
            Self::Subscription(method) => method.lifetime(),
            Self::Demo(method) => method.lifetime(),
            Self::App(method) => method.lifetime(),
            Self::Daemon(_)
            | Self::Chat(_)
            | Self::Removal(_)
            | Self::Recovery(_)
            | Self::Assignment(_)
            | Self::Hold(_)
            | Self::File(_)
            | Self::Ci(_)
            | Self::Branch(_)
            | Self::Approval(_)
            | Self::Port(_)
            | Self::Secret(_)
            | Self::Resource(_)
            | Self::Simulator(_)
            | Self::Container(_)
            | Self::Trigger(_)
            | Self::Hub(_) => Lifetime::Reply,
        }
    }
}

impl From<ExitRequest> for Method {
    fn from(exit: ExitRequest) -> Self {
        Self::Exit(exit)
    }
}

impl From<Operation> for Method {
    fn from(operation: Operation) -> Self {
        Self::Operation(operation)
    }
}

/// Each domain's requests convert into the operation and call carrying them.
macro_rules! from_domains {
    ($($variant:ident($method:ty)),+ $(,)?) => {$(
        impl From<$method> for Operation {
            fn from(method: $method) -> Self {
                Self::$variant(method)
            }
        }

        impl From<$method> for Method {
            fn from(method: $method) -> Self {
                Self::Operation(method.into())
            }
        }
    )+};
}

from_domains!(
    Daemon(DaemonMethod),
    Repository(RepositoryMethod),
    Workspace(WorkspaceMethod),
    Removal(RemovalMethod),
    Recovery(RecoveryMethod),
    Execution(ExecutionMethod),
    Assignment(AssignmentMethod),
    Agent(AgentMethod),
    Chat(ChatMethod),
    Launch(LaunchMethod),
    Queue(QueueMethod),
    Hold(HoldMethod),
    Demo(DemoMethod),
    File(FileMethod),
    Item(ItemMethod),
    Ci(CiMethod),
    Branch(BranchMethod),
    Alert(AlertMethod),
    Operations(OperationMethod),
    Question(QuestionMethod),
    Review(ReviewMethod),
    Approval(ApprovalMethod),
    Port(PortMethod),
    Secret(SecretMethod),
    Resource(ResourceMethod),
    Simulator(SimulatorMethod),
    Container(ContainerMethod),
    Subscription(SubscriptionMethod),
    Trigger(TriggerMethod),
    Hub(HubMethod),
    App(AppMethod),
);

/// How long a request keeps its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    /// Until the reply.
    Reply,
    /// Until either side hangs up, as an execution or a stream does.
    Open,
    /// Until either side hangs up; the client sends it again when the daemon
    /// closes the connection unanswered, as across a restart.
    Resumed,
}
