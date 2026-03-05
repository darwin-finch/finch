import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Refund } from "../models/components/refund.js";
import { RefundCreate } from "../models/components/refundcreate.js";
import { RefundsListRequest, RefundsListResponse } from "../models/operations/refundslist.js";
import { PageIterator } from "../types/operations.js";
export declare class Refunds extends ClientSDK {
    /**
     * List Refunds
     *
     * @remarks
     * List products.
     *
     * **Scopes**: `refunds:read` `refunds:write`
     */
    list(request: RefundsListRequest, options?: RequestOptions): Promise<PageIterator<RefundsListResponse, {
        page: number;
    }>>;
    /**
     * Create Refund
     *
     * @remarks
     * Create a refund.
     *
     * **Scopes**: `refunds:write`
     */
    create(request: RefundCreate, options?: RequestOptions): Promise<Refund | undefined>;
}
//# sourceMappingURL=refunds.d.ts.map