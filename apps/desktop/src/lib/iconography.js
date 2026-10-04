import {
  LayoutGrid, Folder, MonitorSmartphone, Activity, SlidersHorizontal,
  Plus, ArrowRight, ArrowLeft, Check, X, ChevronDown, ChevronRight,
  Cloud, RefreshCw, Laptop, AppWindow, Bookmark, Puzzle, ShieldCheck,
  TriangleAlert, Info, Moon, Sun, Monitor, LogOut, Trash2, Pencil,
  Link, Unplug, Eye, EyeOff, Search, Clock, Download, ExternalLink,
  Mail, LockKeyhole,
} from 'lucide';

// Preserve product aliases so existing actions, selectors and callers remain stable.
// Named imports include only the Lucide geometry used by the product.
export const iconNodes = {
  overview: LayoutGrid, profiles: Folder, devices: MonitorSmartphone,
  activity: Activity, settings: SlidersHorizontal, plus: Plus,
  'arrow-right': ArrowRight, 'arrow-left': ArrowLeft, left: ArrowLeft,
  check: Check, close: X, 'chevron-down': ChevronDown,
  'chevron-right': ChevronRight, cloud: Cloud, refresh: RefreshCw,
  laptop: Laptop, browser: AppWindow, bookmark: Bookmark,
  extension: Puzzle, shield: ShieldCheck, warning: TriangleAlert,
  info: Info, moon: Moon, sun: Sun, system: Monitor, logout: LogOut,
  trash: Trash2, edit: Pencil, link: Link, unplug: Unplug,
  eye: Eye, 'eye-off': EyeOff, search: Search, clock: Clock,
  folder: Folder, download: Download, external: ExternalLink,
  email: Mail, password: LockKeyhole,
};

// Synthetic clicks and keyboard activation update the icon immediately.
export const allowsIconMotion = event => event?.detail > 0;
