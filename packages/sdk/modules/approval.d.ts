/** Environment-neutral approval protocol. Adapters subscribe to approval.request. */
/** `id` is assigned by approval, only for display and logs. */
interface PlyApprovalRequest {
  schema_version: 1;
  id: string;
  tool: "bash";
  command: string;
  cwd: string;
  assessments: unknown[];
  requested_at: number;
}
interface PlyApprovalResponse {
  request_id: string;
  decision: "approved" | "rejected" | "pending";
  reason?: string;
  reference?: string;
  approver?: string;
}

/** Model tool call awaiting execution. Existing bash approval.request stays unchanged. */
interface PlyToolApprovalRequest {
  schema_version: 1;
  id: string;
  tool: string;
  input: unknown;
  cwd: string;
  requested_at: number;
}
