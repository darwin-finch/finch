import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerPortalCustomer } from "../models/components/customerportalcustomer.js";
import { CustomerPortalCustomerUpdate } from "../models/components/customerportalcustomerupdate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalCustomersUpdateSecurity } from "../models/operations/customerportalcustomersupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Customer
 *
 * @remarks
 * Update authenticated customer.
 *
 * **Scopes**: `customer_portal:write`
 */
export declare function customerPortalCustomersUpdate(client: PolarCore, security: CustomerPortalCustomersUpdateSecurity, request: CustomerPortalCustomerUpdate, options?: RequestOptions): APIPromise<Result<CustomerPortalCustomer, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalCustomersUpdate.d.ts.map