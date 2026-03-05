import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomerSession } from "../models/components/customersession.js";
import { CustomerSessionsCreateCustomerSessionCreate } from "../models/operations/customersessionscreate.js";
export declare class CustomerSessions extends ClientSDK {
    /**
     * Create Customer Session
     *
     * @remarks
     * Create a customer session.
     *
     * **Scopes**: `customer_sessions:write`
     */
    create(request: CustomerSessionsCreateCustomerSessionCreate, options?: RequestOptions): Promise<CustomerSession>;
}
//# sourceMappingURL=customersessions.d.ts.map