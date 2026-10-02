export interface User {
  id: string;
  username: string;
  role: 'admin' | 'user';
  uses_default_admin_credentials?: boolean;
  created_at: string;
  libraries_accessible?: string[];
  books_accessible?: string[];
}
