import type { ApiClient, User } from "../api/client";
import { useEffect, useState } from "react";

type AdminRouteProps = {
  apiClient: ApiClient;
};

export function AdminRoute({ apiClient }: AdminRouteProps) {
  const [users, setUsers] = useState<User[]>([]);
  const [model, setModel] = useState<string>("Loading");

  useEffect(() => {
    void apiClient.listUsers().then(setUsers);
    void apiClient.modelConfig().then((config) => setModel(config.default_model));
  }, [apiClient]);

  return (
    <section className="grid-section" id="admin">
      <div className="panel">
        <h2>Users</h2>
        <table>
          <thead>
            <tr>
              <th>Email</th>
              <th>Role</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {users.map((user) => (
              <tr key={user.id}>
                <td>{user.email}</td>
                <td>{user.role}</td>
                <td>{user.status}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="panel">
        <h2>Model configuration</h2>
        <dl>
          <dt>Default model</dt>
          <dd>{model}</dd>
        </dl>
      </div>
    </section>
  );
}
