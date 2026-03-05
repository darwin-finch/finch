import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomerOrder } from "../models/components/customerorder.js";
import { CustomerOrderInvoice } from "../models/components/customerorderinvoice.js";
import { CustomerPortalOrdersGetRequest, CustomerPortalOrdersGetSecurity } from "../models/operations/customerportalordersget.js";
import { CustomerPortalOrdersInvoiceRequest, CustomerPortalOrdersInvoiceSecurity } from "../models/operations/customerportalordersinvoice.js";
import { CustomerPortalOrdersListRequest, CustomerPortalOrdersListResponse, CustomerPortalOrdersListSecurity } from "../models/operations/customerportalorderslist.js";
import { PageIterator } from "../types/operations.js";
export declare class PolarOrders extends ClientSDK {
    /**
     * List Orders
     *
     * @remarks
     * List orders of the authenticated customer.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    list(security: CustomerPortalOrdersListSecurity, request: CustomerPortalOrdersListRequest, options?: RequestOptions): Promise<PageIterator<CustomerPortalOrdersListResponse, {
        page: number;
    }>>;
    /**
     * Get Order
     *
     * @remarks
     * Get an order by ID for the authenticated customer.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    get(security: CustomerPortalOrdersGetSecurity, request: CustomerPortalOrdersGetRequest, options?: RequestOptions): Promise<CustomerOrder>;
    /**
     * Get Order Invoice
     *
     * @remarks
     * Get an order's invoice data.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    invoice(security: CustomerPortalOrdersInvoiceSecurity, request: CustomerPortalOrdersInvoiceRequest, options?: RequestOptions): Promise<CustomerOrderInvoice>;
}
//# sourceMappingURL=polarorders.d.ts.map