

/** What bash submits; approval assigns the request id. */
export type Input = { tool: "bash"; command: string; cwd: string; assessments: unknown[] };

export type Response = { request_id: string; decision: "approved" | "rejected" | "pending"; reason?: string; reference?: string; approver?: string };

export type CommandAuthorization = { status: "approved" | "pending" | "rejected"; request: PlyApprovalRequest; responses: Response[]; reason: string };
export type ToolAuthorization = { status: "not_required" } | { status: "approved" | "pending" | "rejected"; request: PlyToolApprovalRequest; responses: Response[]; reason: string };
