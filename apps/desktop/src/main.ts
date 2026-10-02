import './style.css';
import { mountDesktop } from './bootstrap';

const app = document.querySelector<HTMLElement>('#app');
if (!app) throw new Error('Desktop root element is missing');
void mountDesktop(app, import.meta.env.VITE_MINTCAT_VNEXT_DATA_DIR?.trim());
